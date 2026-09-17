//! Exercises the public LSP flow against a real server process without SAP config.

mod support;

use std::process::{Child, Command, Stdio};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
};

/// Ensure failures also stop the child server.
struct ServerProcess(Child);

#[tokio::test]
async fn startup_flag_controls_lifetime_without_user_config() {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        let directory = tempfile::tempdir().unwrap();
        for daemon in [false, true] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            drop(listener);
            let mut command = Command::new(env!("CARGO_BIN_EXE_abap-language-server"));
            command
                .env("ZIEGE_CONFIG", directory.path().join("missing.toml"))
                .env("ZIEGE_LSP_ADDRESS", address.to_string())
                .env("ZIEGE_IDLE_TIMEOUT_SECONDS", "1")
                .stdin(Stdio::null())
                .stdout(Stdio::null());
            if daemon {
                // CLI values override environment settings; the other branch uses env only.
                command
                    .env("ZIEGE_LSP_ADDRESS", "invalid-address")
                    .env("ZIEGE_IDLE_TIMEOUT_SECONDS", "600")
                    .args([
                        "--daemon",
                        "--address",
                        &address.to_string(),
                        "--idle-timeout-seconds",
                        "1",
                    ]);
            }
            let mut server = ServerProcess(command.spawn().unwrap());
            let stream = loop {
                if let Ok(stream) = TcpStream::connect(address).await {
                    break stream;
                }
                assert!(server.0.try_wait().unwrap().is_none());
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            };
            drop(stream);
            if daemon {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                assert!(server.0.try_wait().unwrap().is_none());
                let stream = TcpStream::connect(address).await.unwrap();
                // Active connections prevent idle shutdown, including reconnects.
                tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
                assert!(server.0.try_wait().unwrap().is_none());
                drop(stream);
            }
            loop {
                if let Some(status) = server.0.try_wait().unwrap() {
                    assert!(status.success());
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        }
    })
    .await
    .expect("server lifecycle timed out");
}

#[tokio::test]
async fn backend_and_view_initialization_are_shared_and_retryable() {
    use abap_lsp::{
        config::{DestinationConfig, DestinationId, FacetConfig, MountConfig, ProjectRoot},
        context::SystemContextStore,
    };
    use std::sync::{Arc, atomic::Ordering};

    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        let backend = support::AdtFixture::start().await;
        let directory = tempfile::tempdir().unwrap();
        let project = ProjectRoot::canonicalize(directory.path()).await.unwrap();
        let store = SystemContextStore::new();
        let id: DestinationId = serde_json::from_value(json!("DEV")).unwrap();
        let config = DestinationConfig {
            url: backend.url.clone(),
            client: "100".into(),
            language: "EN".into(),
            username: "DEVELOPER".into(),
            password: "fixture".into(),
        };
        assert!(store.get(&id).await.is_none());
        let (first, second) = tokio::join!(store.upsert(&id, &config), store.upsert(&id, &config));
        let first = first.unwrap();
        assert!(Arc::ptr_eq(&first, &second.unwrap()));
        assert!(Arc::ptr_eq(&first, &store.get(&id).await.unwrap()));
        assert_eq!(backend.count("/sap/bc/adt/discovery"), 1);

        let explicit_defaults = [MountConfig::SystemLibrary {
            label: "System Library".into(),
            facets: Some(vec![
                FacetConfig::Always("GROUP".into()),
                FacetConfig::Always("TYPE".into()),
            ]),
        }];
        let (one, two) = tokio::join!(
            first.upsert_view(&project, &[]),
            first.upsert_view(&project, &[])
        );
        let original = one.unwrap();
        assert!(Arc::ptr_eq(&original, &two.unwrap()));
        assert!(Arc::ptr_eq(&first.view(&project).await.unwrap(), &original));
        assert!(original.mounts.is_empty());
        let roots = original.tree.children(original.tree.root()).await.unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].label, "System Library");
        assert_eq!(
            backend.count("/sap/bc/adt/repository/informationsystem/virtualfolders/facets"),
            1
        );

        let explicit = first
            .upsert_view(&project, &explicit_defaults)
            .await
            .unwrap();
        assert!(!Arc::ptr_eq(&original, &explicit));
        assert_eq!(explicit.mounts, explicit_defaults);
        let original = explicit;

        let changed_mounts = [MountConfig::SystemLibrary {
            label: "Another view".into(),
            facets: None,
        }];
        backend.fail_next_facets.store(true, Ordering::SeqCst);
        assert!(first.upsert_view(&project, &changed_mounts).await.is_err());
        assert!(Arc::ptr_eq(&first.view(&project).await.unwrap(), &original));
        let changed = first.upsert_view(&project, &changed_mounts).await.unwrap();
        assert_ne!(original.tree.root(), changed.tree.root());
        assert!(Arc::ptr_eq(&first.view(&project).await.unwrap(), &changed));
        assert!(changed.tree.node(original.tree.root()).is_none());
        assert_eq!(backend.count("/sap/bc/adt/discovery"), 1);

        // Identical mounts in another project get an independent tree on the same backend.
        let other_directory = tempfile::tempdir().unwrap();
        let other_project = ProjectRoot::canonicalize(other_directory.path())
            .await
            .unwrap();
        let independent = first
            .upsert_view(&other_project, &changed_mounts)
            .await
            .unwrap();
        assert_ne!(independent.tree.root(), changed.tree.root());

        let mut invalid_config = config.clone();
        invalid_config.url = "not a URL".into();
        assert!(store.upsert(&id, &invalid_config).await.is_err());
        assert!(Arc::ptr_eq(&first, &store.get(&id).await.unwrap()));

        // Credential revisions cannot share clients, views or node ownership.
        let mut new_config = config.clone();
        new_config.password = "changed".into();
        let other = store.upsert(&id, &new_config).await.unwrap();
        assert!(!Arc::ptr_eq(&first, &other));
        assert!(Arc::ptr_eq(&other, &store.get(&id).await.unwrap()));
        assert!(other.view(&project).await.is_none());
        let other_view = other.upsert_view(&project, &[]).await.unwrap();
        assert_ne!(original.tree.root(), other_view.tree.root());
        assert_eq!(backend.count("/sap/bc/adt/discovery"), 2);
        assert!(!Arc::ptr_eq(
            &first,
            &store.upsert(&id, &config).await.unwrap()
        ));
        assert_eq!(backend.count("/sap/bc/adt/discovery"), 3);
    })
    .await
    .expect("context initialization timed out");
}

#[tokio::test]
async fn uri_navigation_uses_the_bound_project_and_restores_paths_after_restarts() {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        let backend = support::AdtFixture::start().await;
        let directory = tempfile::tempdir().unwrap();
        let project_a = directory.path().join("project a");
        let project_b = directory.path().join("project b");
        std::fs::create_dir(&project_a).unwrap();
        std::fs::create_dir(&project_b).unwrap();
        let project_config = "version = 1\n[systems.DEV]\n";
        std::fs::write(project_a.join("ziege.toml"), project_config).unwrap();
        std::fs::write(project_b.join("ziege.toml"),
            "version = 1\n[systems.DEV]\nfolder = 'Other Portal'\nreadonly = true\n[[systems.DEV.mounts]]\nkind = 'systemLibrary'\nfacets = ['TYPE']\n"
        ).unwrap();
        let user_config = directory.path().join("config.toml");
        let destinations = directory.path().join("destinations.toml");
        let destination_config = format!(
            "version = 1\n[destinations.DEV]\nurl = '{}'\nclient = '100'\nusername = 'DEVELOPER'\npassword = 'fixture'\n", backend.url
        );
        std::fs::write(&user_config, "version = 1").unwrap();
        std::fs::write(&destinations, &destination_config).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let mut server = ServerProcess(Command::new(env!("CARGO_BIN_EXE_abap-language-server"))
            .args(["--daemon", "--address", &address.to_string(), "--idle-timeout-seconds", "600"])
            .env("ZIEGE_CONFIG", &user_config)
            .stdin(Stdio::null()).stdout(Stdio::null()).spawn().unwrap());
        let stream = loop {
            if let Ok(stream) = TcpStream::connect(address).await { break stream; }
            assert!(server.0.try_wait().unwrap().is_none());
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };
        let uri_a = url::Url::from_directory_path(&project_a).unwrap();
        let uri_b = url::Url::from_directory_path(&project_b).unwrap();
        let mut stream = BufReader::new(stream);
        let initialize_a = json!({"capabilities": {}, "workspaceFolders": [{"uri": uri_a, "name": "A"}]});
        let initialized = request(&mut stream, 1, "initialize", initialize_a.clone()).await;
        assert!(initialized.get("error").is_none(), "{initialized}");
        let params_a = json!({"uri": "abap://DEV/vfs/"});
        let params_b = params_a.clone();
        let root_a = request(&mut stream, 2, "ziege/fileSystem/readDirectory", params_a.clone()).await;
        let mut second = BufReader::new(TcpStream::connect(address).await.unwrap());
        request(&mut second, 1, "initialize", json!({"capabilities": {}, "rootUri": uri_b,
            "initializationOptions": {"presentation": {"namespaceDelimiter": "slash"}}})).await;
        let root_b = request(&mut second, 2, "ziege/fileSystem/readDirectory", params_b).await;
        assert!(root_a.get("error").is_none(), "{root_a}");
        assert!(root_b.get("error").is_none(), "{root_b}");
        assert_eq!(root_a["result"]["entries"][0]["name"], root_b["result"]["entries"][0]["name"]);
        assert_eq!(root_a["result"]["entries"][0]["uri"], root_b["result"]["entries"][0]["uri"]);
        let mount_uri = root_a["result"]["entries"][0]["uri"].clone();
        assert_eq!(mount_uri, "abap://DEV/vfs/System%20Library/");
        assert_eq!(backend.count("/sap/bc/adt/discovery"), 1);
        assert_eq!(backend.count("/sap/bc/adt/repository/informationsystem/virtualfolders/facets"), 2);

        // The same daemon resumes the selected view even with configuration gone.
        std::fs::remove_file(&user_config).unwrap();
        std::fs::remove_file(&destinations).unwrap();
        std::fs::remove_file(project_a.join("ziege.toml")).unwrap();
        drop(stream);
        let mut stream = BufReader::new(TcpStream::connect(address).await.unwrap());
        request(&mut stream, 1, "initialize", initialize_a.clone()).await;
        let resumed = request(&mut stream, 2, "ziege/fileSystem/readDirectory", params_a.clone()).await;
        assert_eq!(resumed["result"], root_a["result"]);
        let children = request(&mut stream, 3, "ziege/fileSystem/readDirectory",
            json!({"uri": mount_uri, "refresh": true})
        ).await;
        assert_eq!(children["result"]["entries"][0]["name"], "(dmo)flight", "{children}");
        assert_eq!(children["result"]["entries"][0]["uri"], "abap://DEV/vfs/System%20Library/%2FDMO%2FFLIGHT/");
        let foreign = request(&mut stream, 4, "ziege/fileSystem/readDirectory",
            json!({"uri": mount_uri, "projectUri": uri_b})
        ).await;
        assert_eq!(foreign["error"]["code"], -32602);

        let mut refresh = params_a.clone();
        refresh["refresh"] = json!(true);
        let failed_refresh = request(&mut stream, 5, "ziege/fileSystem/readDirectory", refresh.clone()).await;
        assert_eq!(failed_refresh["error"]["code"], -32803);
        let retained = request(&mut stream, 6, "ziege/fileSystem/readDirectory", params_a.clone()).await;
        assert_eq!(retained["result"], root_a["result"]);

        std::fs::write(&user_config, "version = 1").unwrap();
        std::fs::write(&destinations, &destination_config).unwrap();
        std::fs::write(project_a.join("ziege.toml"), format!(
            "{project_config}\n[[systems.DEV.mounts]]\nkind = 'systemLibrary'\nlabel = 'Changed View'\n"
        )).unwrap();
        let cached = request(&mut stream, 7, "ziege/fileSystem/readDirectory", params_a).await;
        assert_eq!(cached["result"], root_a["result"]);
        let changed = request(&mut stream, 8, "ziege/fileSystem/readDirectory", refresh.clone()).await;
        assert_eq!(changed["result"]["entries"][0]["name"], "Changed View", "{changed}");
        assert_eq!(changed["result"]["entries"][0]["uri"], "abap://DEV/vfs/Changed%20View/");
        assert_eq!(backend.count("/sap/bc/adt/discovery"), 1);
        assert_eq!(backend.count("/sap/bc/adt/repository/informationsystem/virtualfolders/facets"), 3);
        let old_children = request(&mut stream, 9, "ziege/fileSystem/readDirectory",
            json!({"uri": mount_uri})
        ).await;
        assert_eq!(old_children["error"]["code"], -32803, "{old_children}");

        std::fs::write(project_a.join("ziege.toml"), project_config).unwrap();
        let reverted = request(&mut stream, 10, "ziege/fileSystem/readDirectory", refresh).await;
        assert_eq!(reverted["result"]["entries"][0]["name"], "System Library");
        assert_eq!(reverted["result"]["entries"][0]["uri"], mount_uri);
        assert_eq!(backend.count("/sap/bc/adt/repository/informationsystem/virtualfolders/facets"), 4);

        // A saved directory URI works without having visited its parents first.
        let deep_uri = "abap://DEV/vfs/System%20Library/%2FDMO%2FFLIGHT/Source%20Code%20Library/Classes/";
        let objects = request(&mut stream, 11, "ziege/fileSystem/readDirectory", json!({"uri": deep_uri})).await;
        assert_eq!(objects["result"]["entries"][0]["name"], "(dmo)cl_flight", "{objects}");
        let object_uri = objects["result"]["entries"][0]["uri"].clone();
        let other_view = request(&mut second, 3, "ziege/fileSystem/readDirectory",
            json!({"uri": "abap://DEV/vfs/System%20Library/%2FDMO%2FFLIGHT/Classes/"})).await;
        assert_eq!(other_view["result"]["entries"][0]["name"], "/dmo/cl_flight", "{other_view}");
        assert_eq!(other_view["result"]["entries"][0]["uri"], "abap://DEV/vfs/System%20Library/%2FDMO%2FFLIGHT/Classes/%2FDMO%2FCL_FLIGHT/");
        let terminal = request(&mut stream, 12, "ziege/fileSystem/readDirectory", json!({"uri": object_uri})).await;
        assert_eq!(terminal["result"]["entries"], json!([]));
        let file = request(&mut stream, 13, "ziege/fileSystem/readDirectory", json!({"uri": "abap://DEV/zcl_example.clas.abap"})).await;
        assert_eq!(file["error"]["code"], -32602);

        drop(stream);
        drop(second);
        server.0.kill().unwrap();
        server.0.wait().unwrap();
        server = ServerProcess(Command::new(env!("CARGO_BIN_EXE_abap-language-server"))
            .args(["--daemon", "--address", &address.to_string()])
            .env("ZIEGE_CONFIG", &user_config)
            .stdin(Stdio::null()).stdout(Stdio::null()).spawn().unwrap());
        let stream = loop {
            if let Ok(stream) = TcpStream::connect(address).await { break stream; }
            assert!(server.0.try_wait().unwrap().is_none());
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };
        let mut stream = BufReader::new(stream);
        request(&mut stream, 1, "initialize", initialize_a).await;
        let restored = request(&mut stream, 2, "ziege/fileSystem/readDirectory", json!({"uri": deep_uri})).await;
        assert_eq!(restored["result"]["entries"][0]["uri"], object_uri, "{restored}");
        assert_eq!(restored["result"], objects["result"]);
        assert_eq!(backend.count("/sap/bc/adt/discovery"), 2);
        let wrong_case = request(&mut stream, 3, "ziege/fileSystem/readDirectory", json!({"uri": "abap://dev/vfs/"})).await;
        assert_eq!(wrong_case["error"]["code"], -32803);
        assert_eq!(backend.count("/sap/bc/adt/discovery"), 2);
    }).await.expect("portal context lifecycle timed out");
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn request(stream: &mut BufReader<TcpStream>, id: u64, method: &str, params: Value) -> Value {
    let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
    stream
        .get_mut()
        .write_all(format!("Content-Length: {}\r\n\r\n{}", body.len(), body).as_bytes())
        .await
        .unwrap();
    loop {
        let mut length = None;
        loop {
            let mut line = String::new();
            assert!(stream.read_line(&mut line).await.unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length:") {
                length = Some(value.trim().parse::<usize>().unwrap());
            }
        }
        let mut body = vec![0; length.unwrap()];
        stream.read_exact(&mut body).await.unwrap();
        let message: Value = serde_json::from_slice(&body).unwrap();
        if message["id"] == id {
            return message;
        }
    }
}

#[tokio::test]
async fn system_listing_defers_loading_connection_configuration_until_browsing() {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.toml");
        std::fs::write(
            directory.path().join("ziege.toml"),
            "version = 1\n[systems.DEMO]\n[systems.'DeV @:/']\nfolder = 'SAP'\n",
        )
        .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let mut server = ServerProcess(
            Command::new(env!("CARGO_BIN_EXE_abap-language-server"))
                .env("ZIEGE_CONFIG", &config)
                .env("ZIEGE_LSP_ADDRESS", address.to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let stream = loop {
            if let Ok(stream) = TcpStream::connect(address).await {
                break stream;
            }
            assert!(
                server.0.try_wait().unwrap().is_none(),
                "server exited before accepting connections"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };
        let mut stream = BufReader::new(stream);
        let uri = url::Url::from_directory_path(directory.path()).unwrap();
        let initialized = request(
            &mut stream,
            1,
            "initialize",
            json!({"capabilities": {}, "workspaceFolders": [{"uri": uri, "name": "project"}],
            "initializationOptions": {"presentation": {"namespaceDelimiter": "parentheses"}}}),
        )
        .await;
        assert!(initialized.get("error").is_none(), "{initialized}");
        assert_eq!(
            initialized["result"]["capabilities"]["experimental"]["ziege"]["protocolVersion"],
            1
        );
        let systems = request(&mut stream, 2, "ziege/project/systems", json!({})).await;
        assert!(systems.get("error").is_none(), "{systems}");
        assert_eq!(
            systems["result"]["systems"],
            json!([
                {"folder": "DEMO", "uri": "abap://DEMO/vfs/"},
                {"folder": "SAP", "uri": "abap://DeV%20%40%3A%2F/vfs/"}
            ])
        );
        assert!(!config.exists());
        std::fs::write(&config, "version = 1\n").unwrap();
        let response = request(
            &mut stream,
            3,
            "ziege/fileSystem/readDirectory",
            json!({"uri": "abap://DEMO/vfs/"}),
        )
        .await;
        assert_eq!(response["error"]["code"], -32803);
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap()
                .starts_with(&format!(
                    "cannot read `{}`:",
                    directory.path().join("destinations.toml").display()
                ))
        );
        assert!(!directory.path().join("destinations.toml").exists());
        let shutdown = request(&mut stream, 4, "shutdown", Value::Null).await;
        assert!(shutdown.get("error").is_none(), "{shutdown}");
    })
    .await
    .expect("configuration loading LSP flow timed out");
}
