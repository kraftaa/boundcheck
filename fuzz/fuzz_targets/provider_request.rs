//! Raw HTTP request bodies, including invalid UTF-8, must never panic the
//! parser, and anything it accepts must survive the state machine and evaluator.
#![no_main]
use boundarycheck::compare;
use boundarycheck::provider::protocol::{parse_request, Protocol};
use boundarycheck::provider::responses;
use boundarycheck::provider::scenario::ScenarioMachine;
use boundarycheck::scenario;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = parse_request(data);
    let _ = responses::parse_request(data, |_| Some(vec![]));
    let def = scenario::find("concurrent-two-tools").unwrap();
    for protocol in Protocol::ALL {
        let mut m = ScenarioMachine::with_protocol("BC_RUN_000001", def, protocol);
        let path = format!("{}{}", m.base_path(), protocol.endpoint());
        // The same body three times walks initial -> results -> completion.
        for _ in 0..3 {
            m.handle("POST", &path, data.to_vec(), false, None);
        }
        let _ = compare::evaluate(&m, &[], vec![]);
    }
});
