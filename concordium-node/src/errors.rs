use rustls::TLSError;
use std;

error_chain! {
    types {
        ErrorWrapper, ErrorKindWrapper, ResultExt, ResultExtWrapper;
    }
    errors {
        QueueingError(t: String) {
            description("can't queue message")
            display("can't queue message: '{}'", t)
        }
        LockingError(t: String) {
            description("can't obtain lock")
            display("can't obtain lock: '{}'", t)
        }
        ProcessControlError(t: String) {
            description("can't stop process")
            display("can't stop process '{}'", t)
        }
        ParseError(t: String) {
            description("can't parse input")
            display("can't parse input '{}", t)
        }
        DuplicatePeerError(t: String) {
            description("duplicate peer")
            display("duplicate peer '{}'", t)
        }
        NetworkError(t: String) {
            description("network error occured")
            display("network error occured '{}", t)
        }
        DNSError(t: String) {
            description("dns error occured")
            display("dns error occured '{}'", t)
        }
    }
    foreign_links {
        RustlsInternalError(TLSError);
        InternalIOError(std::io::Error);
    }
}

impl<T> From<std::sync::PoisonError<T>> for ErrorWrapper {
    fn from(err: std::sync::PoisonError<T>) -> Self {
        use std::error::Error;

        Self::from_kind(ErrorKindWrapper::LockingError(err.description().to_string()))
    }
}