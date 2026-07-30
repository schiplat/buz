use super::{step_log_base, LOG_ECHO, LOG_LOGIN, LOG_STREAM_START, LOG_STEP_ERROR};

#[test]
fn step_log_base_isolates_steps() {
    assert_eq!(step_log_base(0), 0);
    assert_eq!(step_log_base(1), 10_000);
    assert_eq!(step_log_base(2), 20_000);
}

#[test]
fn login_echo_stream_do_not_collide() {
    let base = step_log_base(0);
    let login = base + LOG_LOGIN;
    let echo = base + LOG_ECHO;
    let stream0 = base + LOG_STREAM_START;
    let err = base + LOG_STEP_ERROR;
    assert_ne!(login, echo);
    assert_ne!(echo, stream0);
    assert_ne!(login, stream0);
    assert!(err > stream0);
}
