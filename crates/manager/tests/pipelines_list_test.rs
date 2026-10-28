use manager::api::pipelines::job_success_rate;

#[test]
fn success_rate_none_when_no_jobs() {
    assert_eq!(job_success_rate(0, 0), None);
    assert_eq!(job_success_rate(5, 0), None);
}

#[test]
fn success_rate_all_success() {
    assert_eq!(job_success_rate(3, 3), Some(1.0));
}

#[test]
fn success_rate_partial() {
    let rate = job_success_rate(2, 4).expect("rate");
    assert!((rate - 0.5).abs() < f64::EPSILON);
}

#[test]
fn success_rate_zero_success() {
    assert_eq!(job_success_rate(0, 10), Some(0.0));
}
