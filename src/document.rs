use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchDocument {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<String>,
    #[serde(flatten)]
    pub fields: serde_json::Map<String, serde_json::Value>,
}

impl SearchDocument {
    pub fn new(id: impl Into<String>, fields: impl Into<serde_json::Value>) -> crate::Result<Self> {
        let fields = match fields.into() {
            serde_json::Value::Object(fields) => fields,
            _ => {
                return Err(crate::ScoutError::InvalidResult(
                    "document fields must be a JSON object".to_string(),
                ))
            }
        };
        Ok(Self {
            id: id.into(),
            index: None,
            fields,
        })
    }

    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.fields.get(key)
    }

    pub fn set(&mut self, key: impl Into<String>, value: impl Into<serde_json::Value>) {
        self.fields.insert(key.into(), value.into());
    }
}
