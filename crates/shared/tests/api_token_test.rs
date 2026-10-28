use shared::api_token::*;

#[test]
fn generates_prefixed_random_tokens() {
    let a = generate_token();
    let b = generate_token();
    assert!(a.starts_with(API_TOKEN_PREFIX));
    assert_ne!(a, b);
    assert_eq!(a.len(), API_TOKEN_PREFIX.len() + 64);
}

#[test]
fn hash_is_stable_and_irreversible() {
    let t = generate_token();
    assert_eq!(hash_token(&t), hash_token(&t));
    assert_ne!(hash_token(&t), t);
    assert_eq!(hash_token(&t).len(), 64);
}

#[test]
fn display_prefix_is_short() {
    let t = generate_token();
    let p = display_prefix(&t);
    assert!(p.starts_with(API_TOKEN_PREFIX));
    assert!(p.ends_with('…'));
    assert!(p.len() < 15);
}
