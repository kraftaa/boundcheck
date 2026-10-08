//! Raw bytes on the fake provider's TCP socket: the HTTP/1.1 request parser
//! (headers, Content-Length, chunked bodies, pipelining) must never panic.
#![no_main]
use boundarycheck::provider::server::parse_requests;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    for (_, _, len, truncated) in parse_requests(data) {
        assert!(len <= 32 * 1024 * 1024 && (!truncated || len == 32 * 1024 * 1024));
    }
});
