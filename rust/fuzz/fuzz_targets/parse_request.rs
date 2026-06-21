// Fuzz target: the real request-line/header state machine (`got_request`),
// the function the historical CVEs actually exercised — NOT `parse_method`
// (which only pulls the first word and is CVE-irrelevant).
//
// got_request signature (rust/crates/thttpd-http/src/parse.rs:14):
//   pub fn got_request(read_buf, checked_idx, read_idx, initial_state)
//     -> (GotRequest, usize, ParseState)
//
// Must never panic / abort on arbitrary bytes. Start the FSM at the beginning
// of the buffer and let it consume whatever it can.
#![no_main]
use libfuzzer_sys::fuzz_target;
use thttpd_http::parse::got_request;
use thttpd_http::parse_state::ParseState;

fuzz_target!(|data: &[u8]| {
    let _ = got_request(data, 0, data.len(), ParseState::FirstWord);
});
