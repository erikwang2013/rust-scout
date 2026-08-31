use serde::{Deserialize, Serialize};

use crate::SearchDocument;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchBuilder {
    #[serde(default)]
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<String>,
    #[serde(default)]
    pub wheres: Vec<Where>,
    #[serde(default)]
    pub where_ins: Vec<(String, Vec<serde_json::Value>)>,
    #[serde(default)]
    pub where_not_ins: Vec<(String, Vec<serde_json::Value>)>,
    #[serde(default)]
    pub orders: Vec<Order>,
    #[serde(default)]
    pub options: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub take: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Where {
    pub field: String,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub field: String,
    pub desc: bool,
}

impl Default for SearchBuilder {
    fn default() -> Self {
        Self {
            query: String::new(),
            index: None,
            wheres: Vec::new(),
            where_ins: Vec::new(),
            where_not_ins: Vec::new(),
            orders: Vec::new(),
            options: serde_json::Value::Object(Default::default()),
            take: None,
            skip: None,
        }
    }
}

impl SearchBuilder {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            ..Self::default()
        }
    }

    pub fn within(mut self, index: impl Into<String>) -> Self {
        self.index = Some(index.into());
        self
    }

    pub fn where_field(
        mut self,
        field: impl Into<String>,
        value: impl Into<serde_json::Value>,
    ) -> Self {
        self.wheres.push(Where {
            field: field.into(),
            value: value.into(),
        });
        self
    }

    pub fn where_in(
        mut self,
        field: impl Into<String>,
        values: impl IntoIterator<Item = impl Into<serde_json::Value>>,
    ) -> Self {
        let values = values.into_iter().map(Into::into).collect();
        self.where_ins.push((field.into(), values));
        self
    }

    pub fn where_not_in(
        mut self,
        field: impl Into<String>,
        values: impl IntoIterator<Item = impl Into<serde_json::Value>>,
    ) -> Self {
        let values = values.into_iter().map(Into::into).collect();
        self.where_not_ins.push((field.into(), values));
        self
    }

    pub fn order_by(mut self, field: impl Into<String>, desc: bool) -> Self {
        self.orders.push(Order {
            field: field.into(),
            desc,
        });
        self
    }

    pub fn take(mut self, limit: usize) -> Self {
        self.take = Some(limit);
        self
    }

    pub fn skip(mut self, offset: usize) -> Self {
        self.skip = Some(offset);
        self
    }

    pub fn option(mut self, key: impl Into<String>, value: impl Into<serde_json::Value>) -> Self {
        if let serde_json::Value::Object(ref mut map) = self.options {
            map.insert(key.into(), value.into());
        }
        self
    }

    pub fn matches(&self, doc: &SearchDocument) -> bool {
        if !self.query.trim().is_empty() {
            let needle = self.query.trim().to_lowercase();
            let haystack = serde_json::to_string(&doc.fields)
                .unwrap_or_default()
                .to_lowercase();
            if !haystack.contains(&needle) {
                return false;
            }
        }

        for where_ in &self.wheres {
            if doc.get(&where_.field) != Some(&where_.value) {
                return false;
            }
        }

        for (field, values) in &self.where_ins {
            let value = doc.get(field);
            if !values.iter().any(|v| value == Some(v)) {
                return false;
            }
        }

        for (field, values) in &self.where_not_ins {
            let value = doc.get(field);
            if values.iter().any(|v| value == Some(v)) {
                return false;
            }
        }

        true
    }

    pub fn sort_hits(&self, hits: &mut Vec<crate::SearchHit>) {
        if self.orders.is_empty() {
            hits.sort_by(|a, b| a.id.cmp(&b.id));
            return;
        }
        hits.sort_by(|a, b| {
            for order in &self.orders {
                let left = a.source.get(&order.field).cloned();
                let right = b.source.get(&order.field).cloned();
                let cmp = order_cmp(&left, &right);
                if cmp != std::cmp::Ordering::Equal {
                    return if order.desc { cmp.reverse() } else { cmp };
                }
            }
            a.id.cmp(&b.id)
        });
    }
}

fn order_cmp(a: &Option<serde_json::Value>, b: &Option<serde_json::Value>) -> std::cmp::Ordering {
    match (a, b) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(a), Some(b)) => value_cmp(a, b),
    }
}

fn value_cmp(a: &serde_json::Value, b: &serde_json::Value) -> std::cmp::Ordering {
    use serde_json::Value;
    match (a, b) {
        (Value::Null, Value::Null) => std::cmp::Ordering::Equal,
        (Value::Null, _) => std::cmp::Ordering::Less,
        (_, Value::Null) => std::cmp::Ordering::Greater,
        (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
        (Value::Number(a), Value::Number(b)) => number_cmp(a, b),
        (Value::String(a), Value::String(b)) => a.cmp(b),
        _ => a.to_string().cmp(&b.to_string()),
    }
}

fn number_cmp(a: &serde_json::Number, b: &serde_json::Number) -> std::cmp::Ordering {
    match (a.as_i64(), b.as_i64()) {
        (Some(a), Some(b)) => a.cmp(&b),
        _ => {
            let a = a.as_f64().unwrap_or(f64::NAN);
            let b = b.as_f64().unwrap_or(f64::NAN);
            if a.is_nan() && b.is_nan() {
                std::cmp::Ordering::Equal
            } else {
                a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Less)
            }
        }
    }
}
