use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct FieldError {
    pub field: String,
    pub message: String,
}

#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub code: String,
    pub message: String,
    pub fields: Vec<FieldError>,
}

impl ApiError {
    pub fn new(status: u16, code: &str, message: impl Into<String>) -> Self {
        Self {
            status,
            code: code.to_string(),
            message: message.into(),
            fields: Vec::new(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(400, "bad_request", message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(404, "not_found", message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(409, "conflict", message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(500, "internal", message)
    }

    pub fn with_field(mut self, field: &str, message: impl Into<String>) -> Self {
        self.fields.push(FieldError {
            field: field.to_string(),
            message: message.into(),
        });
        self
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "error": {
                "code": self.code,
                "message": self.message,
                "fields": self.fields,
            }
        })
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
