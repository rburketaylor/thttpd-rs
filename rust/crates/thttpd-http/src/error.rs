//! HTTP error types for thttpd.

/// HTTP error status with context for error page generation.
#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("Bad Request")]
    BadRequest,
    #[error("Unauthorized")]
    Unauthorized { realm: String },
    #[error("Forbidden")]
    Forbidden,
    #[error("Not Found")]
    NotFound,
    #[error("Request Timeout")]
    RequestTimeout,
    #[error("Internal Server Error")]
    InternalServerError,
    #[error("Not Implemented")]
    NotImplemented,
    #[error("Service Unavailable")]
    ServiceUnavailable,
}

impl HttpError {
    /// Returns the HTTP status code for this error.
    pub fn status_code(&self) -> u16 {
        match self {
            HttpError::BadRequest => 400,
            HttpError::Unauthorized { .. } => 401,
            HttpError::Forbidden => 403,
            HttpError::NotFound => 404,
            HttpError::RequestTimeout => 408,
            HttpError::InternalServerError => 500,
            HttpError::NotImplemented => 501,
            HttpError::ServiceUnavailable => 503,
        }
    }

    /// Returns the HTTP status text for this error.
    pub fn status_text(&self) -> &'static str {
        match self {
            HttpError::BadRequest => "Bad Request",
            HttpError::Unauthorized { .. } => "Unauthorized",
            HttpError::Forbidden => "Forbidden",
            HttpError::NotFound => "Not Found",
            HttpError::RequestTimeout => "Request Timeout",
            HttpError::InternalServerError => "Internal Server Error",
            HttpError::NotImplemented => "Not Implemented",
            HttpError::ServiceUnavailable => "Service Unavailable",
        }
    }

    /// Returns a short HTML title for the error page.
    pub fn title(&self) -> &'static str {
        match self {
            HttpError::BadRequest => "Bad Request",
            HttpError::Unauthorized { .. } => "Unauthorized",
            HttpError::Forbidden => "Forbidden",
            HttpError::NotFound => "Not Found",
            HttpError::RequestTimeout => "Request Timeout",
            HttpError::InternalServerError => "Internal Server Error",
            HttpError::NotImplemented => "Not Implemented",
            HttpError::ServiceUnavailable => "Service Unavailable",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_codes() {
        assert_eq!(HttpError::BadRequest.status_code(), 400);
        assert_eq!(
            HttpError::Unauthorized {
                realm: "test".into()
            }
            .status_code(),
            401
        );
        assert_eq!(HttpError::Forbidden.status_code(), 403);
        assert_eq!(HttpError::NotFound.status_code(), 404);
        assert_eq!(HttpError::RequestTimeout.status_code(), 408);
        assert_eq!(HttpError::InternalServerError.status_code(), 500);
        assert_eq!(HttpError::NotImplemented.status_code(), 501);
        assert_eq!(HttpError::ServiceUnavailable.status_code(), 503);
    }
}
