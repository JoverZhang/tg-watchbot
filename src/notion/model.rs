use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize, Debug)]
pub struct DatabaseProperty {
    pub id: String,

    #[allow(dead_code)]
    #[serde(rename = "type")]
    pub typ: String,
}

#[derive(Deserialize, Debug)]
pub struct RetrieveDatabaseResp {
    pub id: String,

    #[allow(dead_code)]
    pub title: Vec<Value>,

    pub properties: std::collections::HashMap<String, DatabaseProperty>,
}
