//! Minimal HTTP fixture for exercising real ZADT clients and ZVFS initialization.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    task::JoinHandle,
};

const DISCOVERY: &str = r#"
<app:service xmlns:app="http://www.w3.org/2007/app" xmlns:atom="http://www.w3.org/2005/Atom">
  <app:workspace>
    <atom:title>Repository</atom:title>
    <app:collection href="/sap/bc/adt/repository/informationsystem/virtualfolders/facets">
      <atom:category term="facets" scheme="http://www.sap.com/adt/categories/repository/virtualfolders" />
    </app:collection>
    <app:collection href="/sap/bc/adt/repository/informationsystem/virtualfolders/contents">
      <app:accept>application/vnd.sap.adt.repository.virtualfolders.result.v1+xml</app:accept>
      <atom:category term="contents" scheme="http://www.sap.com/adt/categories/repository/virtualfolders" />
    </app:collection>
  </app:workspace>
</app:service>"#;

const FACETS: &str = r#"
<vf:facets xmlns:vf="http://www.sap.com/adt/ris/facets">
  <vf:facet key="group" displayName="Group" description="Group"
    isHierarchical="false" isForFiltering="true" isForStructuring="true" />
  <vf:facet key="type" displayName="Type" description="Type"
    isHierarchical="false" isForFiltering="true" isForStructuring="true" />
</vf:facets>"#;

const PACKAGES: &str = r#"<vfs:virtualFoldersResult xmlns:vfs="http://www.sap.com/adt/ris/virtualFolders" objectCount="1">
  <vfs:virtualFolder name="/DMO/FLIGHT" displayName="/DMO/FLIGHT" facet="PACKAGE"
    uri="/sap/bc/adt/packages/%2fdmo%2fflight" counter="1" hasChildrenOfSameFacet="false" />
</vfs:virtualFoldersResult>"#;
const GROUPS: &str = r#"<vfs:virtualFoldersResult xmlns:vfs="http://www.sap.com/adt/ris/virtualFolders" objectCount="1">
  <vfs:virtualFolder name="SOURCE_LIBRARY" displayName="Source Code Library" facet="GROUP" counter="1" hasChildrenOfSameFacet="false" />
</vfs:virtualFoldersResult>"#;
const TYPES: &str = r#"<vfs:virtualFoldersResult xmlns:vfs="http://www.sap.com/adt/ris/virtualFolders" objectCount="1">
  <vfs:virtualFolder name="CLAS" displayName="Classes" facet="TYPE" counter="1" hasChildrenOfSameFacet="false" />
</vfs:virtualFoldersResult>"#;
const OBJECTS: &str = r#"<vfs:virtualFoldersResult xmlns:vfs="http://www.sap.com/adt/ris/virtualFolders" objectCount="1">
  <vfs:object name="/DMO/CL_FLIGHT" package="/DMO/FLIGHT" type="CLAS/OC"
    uri="/sap/bc/adt/oo/classes/%2fdmo%2fcl_flight" expandable="false" text="Flight" />
</vfs:virtualFoldersResult>"#;

pub struct AdtFixture {
    pub url: String,
    pub fail_next_facets: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}

impl AdtFixture {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let fail_next_facets = Arc::new(AtomicBool::new(false));
        let fail = fail_next_facets.clone();
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = BufReader::new(stream);
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                let target = line.split_whitespace().nth(1).unwrap().to_owned();
                let path = target.split('?').next().unwrap();
                let mut length = 0;
                loop {
                    line.clear();
                    assert!(stream.read_line(&mut line).await.unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((key, value)) = line.split_once(':')
                        && key.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                }
                let mut body = vec![0; length];
                stream.read_exact(&mut body).await.unwrap();
                let body = String::from_utf8(body).unwrap();
                recorded.lock().unwrap().push(path.to_owned());
                let (status, body) = match path {
                    "/sap/bc/adt/discovery" => ("200 OK", DISCOVERY),
                    "/sap/bc/adt/core/discovery" => (
                        "200 OK",
                        r#"<app:service xmlns:app="http://www.w3.org/2007/app" />"#,
                    ),
                    "/sap/bc/adt/repository/informationsystem/virtualfolders/facets" => {
                        if fail.swap(false, Ordering::SeqCst) {
                            ("500 Internal Server Error", "fixture failure")
                        } else {
                            ("200 OK", FACETS)
                        }
                    }
                    "/sap/bc/adt/repository/informationsystem/virtualfolders/contents" => (
                        "200 OK",
                        if body.contains("<vfs:facet>PACKAGE</vfs:facet>") {
                            PACKAGES
                        } else if body.contains("<vfs:facet>GROUP</vfs:facet>") {
                            GROUPS
                        } else if body.contains("<vfs:facet>TYPE</vfs:facet>") {
                            TYPES
                        } else {
                            OBJECTS
                        },
                    ),
                    _ => ("404 Not Found", "unexpected fixture request"),
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/xml\r\nx-csrf-token: fixture-token\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .get_mut()
                    .write_all(response.as_bytes())
                    .await
                    .unwrap();
                stream.get_mut().shutdown().await.unwrap();
            }
        });
        Self {
            url,
            fail_next_facets,
            requests,
            task,
        }
    }

    pub fn count(&self, path: &str) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.as_str() == path)
            .count()
    }
}

impl Drop for AdtFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
