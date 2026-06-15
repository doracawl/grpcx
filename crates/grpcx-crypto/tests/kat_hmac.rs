//! HMAC known-answer tests — RFC 4231.

use grpcx_crypto::{Hmac, HmacError, Sha256, Sha384, Sha512};

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn from_hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

// RFC 4231 Test Case 1: 20-byte key (0x0b × 20), "Hi There".
#[test]
fn rfc4231_case1_sha256() {
    let key = vec![0x0b; 20];
    let mut h = Hmac::<Sha256>::new(&key);
    h.update(b"Hi There");
    let mut tag = [0u8; 32];
    h.sign(&mut tag);
    assert_eq!(
        hex(&tag),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );
}

#[test]
fn rfc4231_case1_sha384() {
    let key = vec![0x0b; 20];
    let mut h = Hmac::<Sha384>::new(&key);
    h.update(b"Hi There");
    let mut tag = [0u8; 48];
    h.sign(&mut tag);
    assert_eq!(
        hex(&tag),
        "afd03944d84895626b0825f4ab46907f15f9dadbe4101ec682aa034c7cebc59cfaea9ea9076ede7f4af152e8b2fa9cb6"
    );
}

#[test]
fn rfc4231_case1_sha512() {
    let key = vec![0x0b; 20];
    let mut h = Hmac::<Sha512>::new(&key);
    h.update(b"Hi There");
    let mut tag = [0u8; 64];
    h.sign(&mut tag);
    assert_eq!(
        hex(&tag),
        "87aa7cdea5ef619d4ff0b4241a1d6cb02379f4e2ce4ec2787ad0b30545e17cdedaa833b7d6b8a702038b274eaea3f4e4be9d914eeb61f1702e696c203a126854"
    );
}

#[test]
fn verify_constant_time_accepts_correct_tag() {
    let key = vec![0x0b; 20];
    let mut h1 = Hmac::<Sha256>::new(&key);
    h1.update(b"Hi There");
    let expected = from_hex("b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7");

    let mut h2 = Hmac::<Sha256>::new(&key);
    h2.update(b"Hi There");
    h2.verify(&expected).expect("MAC verify");
}

#[test]
fn verify_rejects_wrong_tag() {
    let key = vec![0x0b; 20];
    let mut h = Hmac::<Sha256>::new(&key);
    h.update(b"Hi There");
    let wrong = vec![0xff; 32];
    assert_eq!(h.verify(&wrong), Err(HmacError::BadMac));
}

#[test]
fn verify_rejects_bad_length() {
    let key = vec![0x0b; 20];
    let mut h = Hmac::<Sha256>::new(&key);
    h.update(b"Hi There");
    let short = vec![0u8; 16];
    assert_eq!(h.verify(&short), Err(HmacError::BadLength));
}

// Cross-hash verify coverage — confirms the TypeId dispatch reaches
// the Sha384 / Sha512 hmac_alg arms (not just Sha256).
#[test]
fn verify_sha384_accepts() {
    let key = vec![0x0b; 20];
    let mut h1 = Hmac::<Sha384>::new(&key);
    h1.update(b"Hi There");
    let expected = from_hex(
        "afd03944d84895626b0825f4ab46907f15f9dadbe4101ec682aa034c7cebc59cfaea9ea9076ede7f4af152e8b2fa9cb6",
    );
    let mut h2 = Hmac::<Sha384>::new(&key);
    h2.update(b"Hi There");
    h2.verify(&expected).expect("MAC verify");
}

#[test]
fn verify_sha512_accepts() {
    let key = vec![0x0b; 20];
    let mut h1 = Hmac::<Sha512>::new(&key);
    h1.update(b"Hi There");
    let expected = from_hex(
        "87aa7cdea5ef619d4ff0b4241a1d6cb02379f4e2ce4ec2787ad0b30545e17cdedaa833b7d6b8a702038b274eaea3f4e4be9d914eeb61f1702e696c203a126854",
    );
    let mut h2 = Hmac::<Sha512>::new(&key);
    h2.update(b"Hi There");
    h2.verify(&expected).expect("MAC verify");
}
