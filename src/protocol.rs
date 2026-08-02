use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum NamespaceDelimiter {
    #[default]
    Parentheses,
    Slash,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PresentationOptions {
    #[serde(default)]
    pub namespace_delimiter: NamespaceDelimiter,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InitializationOptions {
    #[serde(default)]
    pub presentation: PresentationOptions,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSystemsParams {
    pub project_uri: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSystemsResult {
    pub systems: Vec<ProjectSystem>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSystem {
    pub name: String,
    pub folder: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadDirectoryParams {
    pub project_uri: String,
    pub system: String,
    #[serde(default)]
    pub parent_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadDirectoryResult {
    pub entries: Vec<FileSystemEntry>,
    pub modifiable: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSystemEntry {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub entry_type: EntryType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub virtual_folder: bool,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum EntryType {
    File,
    Directory,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadFileParams {
    pub project_uri: String,
    pub system: String,
    pub resource_id: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadFileResult {
    pub content: String,
    pub language_id: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    pub writable: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectCreationOptionsParams {
    pub project_uri: String,
    pub system: String,
    #[serde(default)]
    pub parent_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectCreationOptionsResult {
    pub supported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    pub forms: Vec<ObjectCreationForm>,
    pub transports: Vec<CreationChoice>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectCreationForm {
    pub id: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub questions: Vec<CreationQuestion>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreationQuestion {
    pub id: String,
    pub kind: CreationQuestionKind,
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<CreationChoice>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CreationQuestionKind {
    Input,
    Select,
    Confirm,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreationChoice {
    pub id: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshTransportsParams {
    pub project_uri: String,
    pub system: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub context_token: String,
    pub revision: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshTransportsResult {
    pub supported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    pub transports: Vec<CreationChoice>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn initialization_options_default_to_parentheses() {
        let options: InitializationOptions = serde_json::from_value(json!({})).unwrap();
        assert_eq!(
            NamespaceDelimiter::Parentheses,
            options.presentation.namespace_delimiter
        );
        let options: InitializationOptions = serde_json::from_value(json!({
            "presentation": { "namespaceDelimiter": "slash" }
        }))
        .unwrap();
        assert_eq!(
            NamespaceDelimiter::Slash,
            options.presentation.namespace_delimiter
        );
    }

    #[test]
    fn unsupported_creation_results_have_an_explicit_wire_shape() {
        let result = ObjectCreationOptionsResult {
            supported: false,
            message: Some("Object creation is not available".to_owned()),
            context_token: None,
            revision: None,
            forms: Vec::new(),
            transports: Vec::new(),
        };
        assert_eq!(
            json!({
                "supported": false,
                "message": "Object creation is not available",
                "forms": [],
                "transports": []
            }),
            serde_json::to_value(result).unwrap()
        );
    }
}
