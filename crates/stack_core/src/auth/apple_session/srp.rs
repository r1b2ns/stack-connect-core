//! The SRP-6a client Apple's `signin/init` + `signin/complete` expect, byte for
//! byte the `fastlane-sirp` 1.0.0 gem as spaceship (`client.rb`, `do_sirp`)
//! drives it, plus Apple's password derivation (`pbkdf2` in `client.rb`).
//!
//! An off-the-shelf SRP crate does not fit: Apple's variant differs from RFC
//! 5054 / the `srp` crate in several places, all reproduced here on purpose:
//!
//! - `x = SHA256(salt ‖ SHA256(":" ‖ key))`, **without** the username
//!   (`calc_x_hex`), where `key` is the PBKDF2 output, not the password;
//! - `H(…)` left-pads every argument to the width of `N` (256 bytes),
//!   including `g` in `H(g)` for `M` (`H` in `sirp.rb`);
//! - `K = SHA256(S)` over the **minimal** big-endian bytes of `S`, not padded;
//! - `A`, `H(N) xor H(g)` and `H(A, M, K)` are sent or hashed in their minimal
//!   big-endian form (`num_to_hex` drops leading zero bytes), while `B` and the
//!   salt are hashed exactly as Apple sent them;
//! - `M2 = H(A ‖ M ‖ K)` is likewise minimal, so it can be shorter than 32
//!   bytes.
//!
//! The test vectors at the bottom were produced by the gem itself (see
//! `VECTORS`).
//!
//! Secret handling: the password, its digest, the PBKDF2 key, the inner `x`
//! digest and the private ephemeral `a` bytes live in [`Zeroizing`] buffers.
//! The big-number intermediates (`a`, `x`, `S`) are `num_bigint::BigUint`,
//! which cannot be wiped and whose `modpow` is not constant-time; both are
//! the same trade-off the Ruby gem makes and are noted here so a later change
//! can move to a constant-time big-number crate.

use std::fmt;
use std::sync::LazyLock;

use num_bigint::BigUint;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::error::AppleSignInError;

/// The RFC 5054 2048-bit group prime (`Ng(2048)` in `parameters.rb`).
const N_HEX: &str = "AC6BDB41324A9A9BF166DE5E1389582FAF72B6651987EE07FC3192943DB56050\
A37329CBB4A099ED8193E0757767A13DD52312AB4B03310DCD7F48A9DA04FD50\
E8083969EDB767B0CF6095179A163AB3661A05FBD5FAAAE82918A9962F0B93B8\
55F97993EC975EEAA80D740ADBF4FF747359D041D5C33EA71D281E446B14773B\
CA97B43A23FB801676BD207A436C6481F1D2B9078717461A5B9D32E688F87748\
544523B524B0D57D5EA77A2775D2ECFA032CFBDBF52FB3786160279004E57AE6\
AF874E7303CE53299CCC041C7BC308D82A5698F3A8D0C38271AE35F8E9DBFBB6\
94B5C803D89F7AE435DE236D525F54759B65E372FCD68EF20FA7111F9E4AFF73";

/// Width of `N` in bytes; every `H(…)` argument is left-padded to it.
const N_LEN: usize = 256;

/// `N` decoded at compile time (an invalid literal fails the build).
const N_BYTES: [u8; N_LEN] = decode_hex(N_HEX);

/// The generator.
const G: u8 = 2;

/// `g` left-padded to the width of `N`, as `H(…)` hashes it.
const G_PADDED: [u8; N_LEN] = {
    let mut g = [0u8; N_LEN];
    g[N_LEN - 1] = G;
    g
};

/// The PBKDF2 output length spaceship asks for (`key_length = 32`).
pub(crate) const DERIVED_KEY_LEN: usize = 32;

/// Iteration counts above this are refused before any work is done: Apple
/// sends counts in the thousands, and a count from the wire must not be able
/// to pin a CPU for minutes.
pub(crate) const MAX_ITERATIONS: u32 = 1_000_000;

/// Salts longer than this are refused (Apple's are 16 bytes).
pub(crate) const MAX_SALT_LEN: usize = 1024;

static N: LazyLock<BigUint> = LazyLock::new(|| BigUint::from_bytes_be(&N_BYTES));

/// `k = H(N, g)`.
static K: LazyLock<BigUint> = LazyLock::new(|| hash_mod_n(&[&N_BYTES, &G_PADDED]));

/// `H(N) xor H(g)` in its minimal big-endian form, as `calc_M` hashes it.
static H_N_XOR_H_G: LazyLock<Vec<u8>> =
    LazyLock::new(|| (hash_mod_n(&[&N_BYTES]) ^ hash_mod_n(&[&G_PADDED])).to_bytes_be());

/// Decodes a hex literal into a fixed-size array at compile time.
const fn decode_hex<const L: usize>(hex: &str) -> [u8; L] {
    const fn nibble(c: u8) -> u8 {
        match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => panic!("invalid hex digit"),
        }
    }
    let bytes = hex.as_bytes();
    assert!(bytes.len() == L * 2, "hex literal has the wrong length");
    let mut out = [0u8; L];
    let mut i = 0;
    while i < L {
        out[i] = (nibble(bytes[2 * i]) << 4) | nibble(bytes[2 * i + 1]);
        i += 1;
    }
    out
}

/// The Apple ID password, held only long enough to derive the SRP key.
///
/// Zeroized on drop; it has no `Display` and its `Debug` is redacted, so it
/// cannot end up in a log line or an error message by accident.
pub struct Password(Zeroizing<String>);

impl Password {
    /// Takes ownership of the password (the `String` is wiped when the
    /// `Password` is dropped).
    pub fn new(password: String) -> Self {
        Self(Zeroizing::new(password))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl From<String> for Password {
    fn from(password: String) -> Self {
        Self::new(password)
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password([redacted])")
    }
}

/// Apple's password-derivation protocols, as `signin/init` names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PasswordProtocol {
    /// `PBKDF2(SHA256(password))`.
    S2k,
    /// `PBKDF2(hex(SHA256(password)))`: the lowercase hex string of the
    /// digest is the PBKDF2 password (legacy).
    S2kFo,
}

impl PasswordProtocol {
    /// The protocols this client offers in `signin/init` (`protocols`).
    pub(crate) const OFFERED: [&'static str; 2] = ["s2k", "s2k_fo"];

    /// Parses the `protocol` Apple picked.
    ///
    /// # Errors
    /// [`AppleSignInError::Srp`] for anything but `s2k` / `s2k_fo`, as
    /// spaceship raises `Unsupported protocol`.
    pub(crate) fn from_apple(protocol: &str) -> Result<Self, AppleSignInError> {
        match protocol {
            "s2k" => Ok(Self::S2k),
            "s2k_fo" => Ok(Self::S2kFo),
            other => Err(AppleSignInError::srp(format!(
                "unsupported password protocol {other:?}"
            ))),
        }
    }
}

/// Derives the 32-byte SRP password key: `PBKDF2-HMAC-SHA256(p, salt,
/// iterations, 32)` where `p` is `SHA256(password)` (`s2k`) or its lowercase
/// hex (`s2k_fo`).
///
/// # Errors
/// [`AppleSignInError::Srp`] for an empty password or salt, a salt over
/// [`MAX_SALT_LEN`], or an iteration count of 0 or over [`MAX_ITERATIONS`].
pub(crate) fn derive_password_key(
    password: &Password,
    salt: &[u8],
    iterations: u32,
    protocol: PasswordProtocol,
) -> Result<Zeroizing<[u8; DERIVED_KEY_LEN]>, AppleSignInError> {
    if password.is_empty() {
        return Err(AppleSignInError::srp("the password is empty"));
    }
    if salt.is_empty() || salt.len() > MAX_SALT_LEN {
        return Err(AppleSignInError::srp(format!(
            "the salt has an unexpected length ({} bytes)",
            salt.len()
        )));
    }
    if iterations == 0 || iterations > MAX_ITERATIONS {
        return Err(AppleSignInError::srp(format!(
            "the iteration count {iterations} is out of range"
        )));
    }

    let digest = Zeroizing::new(<[u8; 32]>::from(Sha256::digest(password.as_bytes())));
    let mut key = Zeroizing::new([0u8; DERIVED_KEY_LEN]);
    match protocol {
        PasswordProtocol::S2k => {
            pbkdf2::pbkdf2_hmac::<Sha256>(digest.as_ref(), salt, iterations, key.as_mut());
        }
        PasswordProtocol::S2kFo => {
            let hex = Zeroizing::new(lower_hex(digest.as_ref()));
            pbkdf2::pbkdf2_hmac::<Sha256>(hex.as_bytes(), salt, iterations, key.as_mut());
        }
    }
    Ok(key)
}

fn lower_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 0x0f)]));
    }
    out
}

/// `H(…)` of `sirp.rb` over arguments already padded to [`N_LEN`] bytes:
/// SHA-256 of their concatenation, taken modulo `N`.
fn hash_mod_n(parts: &[&[u8; N_LEN]]) -> BigUint {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    BigUint::from_bytes_be(&hasher.finalize()) % &*N
}

/// Left-pads `part` with zeros to [`N_LEN`] bytes.
///
/// # Errors
/// A value longer than `N` (the gem raises "Bit width does not match").
fn pad(part: &[u8]) -> Result<[u8; N_LEN], AppleSignInError> {
    let offset = N_LEN.checked_sub(part.len()).ok_or_else(|| {
        AppleSignInError::srp(format!(
            "a {}-byte value does not fit the 2048-bit group",
            part.len()
        ))
    })?;
    let mut out = [0u8; N_LEN];
    out[offset..].copy_from_slice(part);
    Ok(out)
}

/// What `signin/complete` needs from the client.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct SrpProof {
    /// The client evidence `M` (32 bytes).
    pub(crate) m1: Vec<u8>,
    /// `H(A, M, K)`, minimal big-endian (normally 32 bytes, rarely fewer).
    pub(crate) m2: Vec<u8>,
}

impl fmt::Debug for SrpProof {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SrpProof([redacted])")
    }
}

/// One SRP-6a client exchange (one sign-in attempt).
pub(crate) struct SrpClient {
    a: BigUint,
    /// `A = g^a mod N`, minimal big-endian (what `start_authentication`
    /// returns and `signin/init` sends as `a`).
    public: Vec<u8>,
}

impl fmt::Debug for SrpClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SrpClient([redacted])")
    }
}

impl SrpClient {
    /// A client with a fresh random 256-byte private ephemeral `a`
    /// (`SecureRandom.hex(256).hex` in the gem).
    ///
    /// # Errors
    /// [`AppleSignInError::Srp`] when the OS random source fails.
    pub(crate) fn new() -> Result<Self, AppleSignInError> {
        let mut a = Zeroizing::new([0u8; N_LEN]);
        getrandom::getrandom(a.as_mut())
            .map_err(|e| AppleSignInError::srp(format!("no secure random source: {e}")))?;
        Ok(Self::from_private(a.as_ref()))
    }

    /// A client with a given private ephemeral (big-endian bytes).
    pub(crate) fn from_private(a: &[u8]) -> Self {
        let a = BigUint::from_bytes_be(a);
        let public = BigUint::from(G).modpow(&a, &N).to_bytes_be();
        Self { a, public }
    }

    /// `A`, minimal big-endian.
    pub(crate) fn public_key(&self) -> &[u8] {
        &self.public
    }

    /// `process_challenge(user, hex(key), hex(salt), hex(B),
    /// is_password_encrypted: true)` followed by `H_AMK`.
    ///
    /// # Errors
    /// [`AppleSignInError::Srp`] for an empty salt or `B`, a `B` wider than
    /// `N`, `B ≡ 0 (mod N)` or `u = 0` (the gem's SRP-6a safety checks).
    pub(crate) fn process_challenge(
        &self,
        username: &str,
        key: &[u8; DERIVED_KEY_LEN],
        salt: &[u8],
        server_public: &[u8],
    ) -> Result<SrpProof, AppleSignInError> {
        if salt.is_empty() {
            return Err(AppleSignInError::srp("Apple sent an empty salt"));
        }
        if server_public.is_empty() || server_public.len() > N_LEN {
            return Err(AppleSignInError::srp(format!(
                "Apple sent a {}-byte B",
                server_public.len()
            )));
        }
        let n = &*N;
        let b = BigUint::from_bytes_be(server_public);
        if (&b % n) == BigUint::default() {
            return Err(AppleSignInError::srp("Apple sent an invalid B"));
        }

        // x = SHA256(salt ‖ SHA256(":" ‖ key))  (calc_x_hex)
        let inner = Zeroizing::new(<[u8; 32]>::from(
            Sha256::new()
                .chain_update(b":")
                .chain_update(key)
                .finalize(),
        ));
        let x_digest = Zeroizing::new(<[u8; 32]>::from(
            Sha256::new()
                .chain_update(salt)
                .chain_update(inner.as_ref())
                .finalize(),
        ));
        let x = BigUint::from_bytes_be(x_digest.as_ref());

        let u = hash_mod_n(&[&pad(&self.public)?, &pad(server_public)?]);
        if u == BigUint::default() {
            return Err(AppleSignInError::srp("the scrambling parameter is zero"));
        }

        // S = (B - k·g^x) ^ (a + u·x) mod N  (calc_client_S); the subtraction
        // is taken mod N first, as Ruby's `%` does for a negative difference.
        let kgx = (&*K * BigUint::from(G).modpow(&x, n)) % n;
        let base = ((&b % n) + n - kgx) % n;
        let exponent = &self.a + &u * &x;
        let s = Zeroizing::new(base.modpow(&exponent, n).to_bytes_be());
        let k_session = Zeroizing::new(<[u8; 32]>::from(Sha256::digest(s.as_slice())));

        // M = H(H(N) xor H(g) ‖ H(I) ‖ s ‖ A ‖ B ‖ K)  (calc_M)
        let m1 = Sha256::new()
            .chain_update(H_N_XOR_H_G.as_slice())
            .chain_update(Sha256::digest(username.as_bytes()))
            .chain_update(salt)
            .chain_update(&self.public)
            .chain_update(server_public)
            .chain_update(k_session.as_ref())
            .finalize()
            .to_vec();

        // H(A, M, K), minimal big-endian (num_to_hex of the integer).
        let m2_digest = Sha256::new()
            .chain_update(&self.public)
            .chain_update(&m1)
            .chain_update(k_session.as_ref())
            .finalize();
        let m2 = BigUint::from_bytes_be(&m2_digest).to_bytes_be();

        Ok(SrpProof { m1, m2 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;

    /// A test vector produced offline by the `fastlane-sirp` 1.0.0 gem (the
    /// one fastlane 2.232.2 ships) and a verbatim copy of spaceship's
    /// `pbkdf2`, with this script (`ruby srp_vectors.rb`, gems loaded from the
    /// Homebrew fastlane install, no network):
    ///
    /// ```ruby
    /// key = pbkdf2(password, salt, iterations, 32, protocol)
    /// client = SIRP::Client.new(2048)
    /// client.instance_variable_set(:@a, a_hex.hex)     # fixed private a
    /// aa = client.start_authentication
    /// verifier = SIRP::Verifier.new(2048)               # server side, for B
    /// x = verifier.calc_x_hex(to_hex(key), salt_hex, Digest::SHA256)
    /// v = verifier.num_to_hex(verifier.calc_v(x, verifier.N, verifier.g))
    /// verifier.instance_variable_set(:@b, b_hex.hex)    # fixed private b
    /// cp = verifier.get_challenge_and_proof(user, v, salt_hex, aa)
    /// m1 = client.process_challenge(user, to_hex(key), salt_hex,
    ///        cp[:challenge][:B], is_password_encrypted: true)
    /// m2 = client.H_AMK
    /// raise unless verifier.verify_session(cp[:proof], m1) == m2
    /// # printed: Base64.strict_encode64(to_byte(aa)), B, m1, m2 (spaceship's
    /// # own encodings), and to_hex(key)
    /// ```
    ///
    /// `a` is `SHA256(seed)` in hex repeated 8 times (256 bytes), plus
    /// `a_offset`: the two offset vectors were found by searching for an `A`
    /// with a leading zero byte (255 bytes sent) and an `H(A, M, K)` with one
    /// (31-byte `m2`), the minimal-encoding quirks of `num_to_hex`.
    struct Vector {
        name: &'static str,
        user: &'static str,
        password: &'static str,
        protocol: PasswordProtocol,
        iterations: u32,
        salt_b64: &'static str,
        a_seed: &'static str,
        a_offset: u32,
        b_pub_b64: &'static str,
        key_hex: &'static str,
        a_pub_b64: &'static str,
        m1_b64: &'static str,
        m2_b64: &'static str,
    }

    const B1: &str = "KFq55tNyVFx8RnY4IbByCQrE3bpnt0HDIoQQFXrHcP8lfXTZhNJE2jmqrxVwAWXgTJlhc1WwsupIaEidXafy5WfPeXiK68peE2h/TK020w0P+/BRTEweYo6sCAvN8ynQL4l96tYKSGz1OHdM/kSmOWAmyNZzbQGo19CbfxgflDycLy2cAjR4vvOkZ3ZqVvR5KYZX6lehmSrEJcguHWDZhBh7EswWYWVZvgLCpaZ9v3x2RXIpg2grpiVpWt0fF6fvoyGkR9mAsXWG2fkgZzcarqIqE93b6s0wA8413QdFoRz6sgzyvp2p70KlWMZKDFpcUsXwzOqvKqd8wtt4AConng==";

    const VECTORS: [Vector; 4] = [
        Vector {
            name: "s2k",
            user: "dev@example.com",
            password: "correct horse battery staple",
            protocol: PasswordProtocol::S2k,
            iterations: 1000,
            salt_b64: "AQIDBAUGBwgJCgsMDQ4PEA==",
            a_seed: "a-seed-1",
            a_offset: 0,
            b_pub_b64: B1,
            key_hex: "9c01cbdb764ced74d5a71d5fb2e2625faaffac2ef99b2165c9e43f0380109256",
            a_pub_b64: "lmtIypRf5cgVPs3QQQ6zJmpmcGkFpCmJftOtl5kKV5qjzOrqdHZHx0L1P3E2Gu9qC4ElCpROrz6FyLceiKemLL+rkdF6Ej36aOBYqd+JnJrsu4VXQPHdUKVfX+anrZQbXSppYMyWdsC+9YiBT8bj8qmomT9UTFyDDtO7i02A7JnsLGTsvz6Luf8iEb5a9OtHwf+9EEcWLezQpzykTLdOfldqMmowCn1xSrxNKN+p93Mulfw32YB56bSQ3132LwMug61LfTl2ZLmsOVObcpGmuSbrlXzRhhdZCDj/lpnEM37YO+hBr49g5orHK6096qFRP9tnmWTvx20uc5HBVMTNNw==",
            m1_b64: "eoQOtn2nXrNUAUEJSukyyhe/VH7KIv4f0xmMOd/ggUw=",
            m2_b64: "Mya8Xp1XbUXpF9+iShVBVPR6ke1qYtT9M5FTNOHxYtQ=",
        },
        Vector {
            name: "s2k_fo",
            user: "dev@example.com",
            password: "p\u{e4}ssw\u{f6}rd-\u{fc}n\u{ef}code",
            protocol: PasswordProtocol::S2kFo,
            iterations: 2000,
            salt_b64: "/ty6mHZUMhABI0VniavN7w==",
            a_seed: "a-seed-2",
            a_offset: 0,
            b_pub_b64: "PvyYtu6LND2CGkF3r/qhH/TAlwYzp/0PTkAVqjpbUcn534lDTL//rMdfbArri0gqhZuY+AsFPdM8sJODr1skfUxndJXm0qyzM7NQLl7QNKZy695USmJcPij/ULXmCf8BrDsRT3dSus9/oE1fpJEPcVV45857ZVrQXW5SeC4h5cuzDbONDT4dc5Lr/Ze1W0PZ+sAiXGa8cjQpQZ38zLbIJklzlrLEZ9CgF+HHAALVt/jEfyfmNmz2ifw0mxCnwzp10lu/YEv6dExCfmPU4wrp4AgoX7MFfIgzam5eWUmIHwSuptuIoXieFp2GbVDe8gU2pYJRue0zQsLLMBXXG/s9ug==",
            key_hex: "10942b0c67a7669cc457d0e9129ad9044a3025daf4ef78d3d2997af143964e84",
            a_pub_b64: "b5OYJn+qo3YVbvLQ1clpIAWyHUrw06mlyozzDGLzqTbFgysUb4wN954i1Ac1vm7wcJXoifQHKeMaSSgmQY2C/Q8sGTmZOrFBZKedYVhQvaOYghjhcRhWPzbG2U/L7qGjSZebbzX0hwioaBHXxUYRv1gKqVUBFX4x2T5POZhCLikEKxNFItepFoK0p/2gRlbLMyNLqBJ4ZwrlAryHfGvtZskJU30xLCY9O68P1mG8ZUz0P+yaJX6wrvLO3S2+O0xh36cMx2jen7igsSVqi/Y1yTCB6BOQFve1hC59uk23SQURyld8mDU9ryeJ/yp1rb7YKdoexRWVh2DCGFwaFWaH2A==",
            m1_b64: "5HhUC2C8tgJEaZbd0TE5fEplw7aOZ3Ha/nLPXb5Chq8=",
            m2_b64: "KvQM1kMqe1hVHRsb/xwxfyEXi6PFKSZTJ4hAq5HMk+I=",
        },
        Vector {
            name: "short_A",
            user: "dev@example.com",
            password: "correct horse battery staple",
            protocol: PasswordProtocol::S2k,
            iterations: 1000,
            salt_b64: "AQIDBAUGBwgJCgsMDQ4PEA==",
            a_seed: "a-seed-1",
            a_offset: 314,
            b_pub_b64: B1,
            key_hex: "9c01cbdb764ced74d5a71d5fb2e2625faaffac2ef99b2165c9e43f0380109256",
            a_pub_b64: "jd+nQPbRDLtT0vbqI7utu8lxawKG4IEhreqoSupTXuMN8O4Q5LC7pAvkDNCVLLQEUKvNdDrTcb1lvG6XrsuHbsYzm3+w2Ia8FALPmR5tejMdsMxNj5buDei1SkNbH12udaR+5OEHPsue8kWr5FCwNi3EkzLlbect42DddCAlIDg5Mh2/3aMdBC/6anp9lCLkdwi7vh1nJM2zh2y0eLUR9ejDXMYM5gfh6/iYwXw9NasXqw8pY6SWYDM+6QBo8VAhKzNxITxt1wE/Y6H6IuuUMMbtuozP5lWJ2ugMPgvwE7wDKw8pW4v7uf/ejJNZF2fL4wRNhLcWIjxNJaSv+tY7",
            m1_b64: "uhXabpszi9EYTur6AOX92WMf4piqqbssQo7UPpkeDRM=",
            m2_b64: "SdBM3+FnIbsDp8Aep8eMktIKTS63oOiAle/eHYXsK6Y=",
        },
        Vector {
            name: "short_m2",
            user: "dev@example.com",
            password: "correct horse battery staple",
            protocol: PasswordProtocol::S2k,
            iterations: 1000,
            salt_b64: "AQIDBAUGBwgJCgsMDQ4PEA==",
            a_seed: "a-seed-1",
            a_offset: 1_000_022,
            b_pub_b64: B1,
            key_hex: "9c01cbdb764ced74d5a71d5fb2e2625faaffac2ef99b2165c9e43f0380109256",
            a_pub_b64: "KVukOjtTCWeuNsL6e3dU4zqXiRXcuRN9LhlJ1+IuRPPGBYb6SBQe4sZVqBlhi7+N+CvX/wH5IG+L2PjoThgl0CUTOfCWuhAIJIEu8Kp/aYXPxZJ5SqQ2YRlzMFzuPqoBNfLdajixk0JnmHhIGYUfzQMuL1pDN1+etaJ1fuCMM35iV4j33IBNWsZW9VM+TgIjcuy2QUTJNhX3LQf8ZuuUBNJYV8p5DqHaGo4gB2ZHub4wGTgI7U0rD1CbS7Pl0owHHGJoXdMdQ8iADoZf4Y/3ogFHImI5Y8lxNsK2mJrax3vVrwclCGY2vsChen6pG3h8vykfl+A68Q4XbGzIxr29tQ==",
            m1_b64: "zYihYlEHX4WSNb5HN4yDRzEMC2MQcCitEVoukoTnX4k=",
            m2_b64: "pHfZHFYaXZ503EF/M1qzNLg259C1PvQ6I6SuKiUi4Q==",
        },
    ];

    fn b64(text: &str) -> Vec<u8> {
        STANDARD.decode(text).unwrap()
    }

    fn private_a(vector: &Vector) -> Vec<u8> {
        let digest = lower_hex(&Sha256::digest(vector.a_seed.as_bytes()));
        let hex = digest.repeat(8);
        let a = BigUint::parse_bytes(hex.as_bytes(), 16).unwrap() + vector.a_offset;
        a.to_bytes_be()
    }

    fn key_for(vector: &Vector) -> Zeroizing<[u8; 32]> {
        derive_password_key(
            &Password::new(vector.password.to_string()),
            &b64(vector.salt_b64),
            vector.iterations,
            vector.protocol,
        )
        .unwrap()
    }

    #[test]
    fn n_is_the_rfc5054_2048_bit_prime() {
        assert_eq!(N_BYTES[0], 0xAC);
        assert_eq!(N_BYTES[N_LEN - 1], 0x73);
        assert_eq!(N.bits(), 2048);
        assert_eq!(
            lower_hex(&N_BYTES),
            N_HEX.to_ascii_lowercase(),
            "compile-time decoding matches the literal"
        );
    }

    #[test]
    fn password_key_matches_spaceship_pbkdf2_for_both_protocols() {
        for vector in &VECTORS {
            assert_eq!(
                lower_hex(key_for(vector).as_ref()),
                vector.key_hex,
                "{}",
                vector.name
            );
        }
    }

    #[test]
    fn s2k_and_s2k_fo_differ_for_the_same_input() {
        let password = Password::new("password123".to_string());
        let s2k = derive_password_key(&password, b"salt", 1000, PasswordProtocol::S2k).unwrap();
        let fo = derive_password_key(&password, b"salt", 1000, PasswordProtocol::S2kFo).unwrap();
        assert_ne!(s2k.as_ref(), fo.as_ref());
    }

    #[test]
    fn srp_matches_fastlane_sirp_byte_for_byte() {
        for vector in &VECTORS {
            let client = SrpClient::from_private(&private_a(vector));
            assert_eq!(
                client.public_key(),
                b64(vector.a_pub_b64).as_slice(),
                "A of {}",
                vector.name
            );
            let proof = client
                .process_challenge(
                    vector.user,
                    &key_for(vector),
                    &b64(vector.salt_b64),
                    &b64(vector.b_pub_b64),
                )
                .unwrap();
            assert_eq!(proof.m1, b64(vector.m1_b64), "m1 of {}", vector.name);
            assert_eq!(proof.m2, b64(vector.m2_b64), "m2 of {}", vector.name);
        }
    }

    #[test]
    fn minimal_encodings_drop_leading_zero_bytes() {
        let short_a = VECTORS.iter().find(|v| v.name == "short_A").unwrap();
        assert_eq!(
            SrpClient::from_private(&private_a(short_a))
                .public_key()
                .len(),
            255
        );
        let short_m2 = VECTORS.iter().find(|v| v.name == "short_m2").unwrap();
        let proof = SrpClient::from_private(&private_a(short_m2))
            .process_challenge(
                short_m2.user,
                &key_for(short_m2),
                &b64(short_m2.salt_b64),
                &b64(short_m2.b_pub_b64),
            )
            .unwrap();
        assert_eq!(proof.m1.len(), 32);
        assert_eq!(proof.m2.len(), 31);
    }

    #[test]
    fn a_random_client_has_a_public_key_below_n() {
        let client = SrpClient::new().unwrap();
        let public = BigUint::from_bytes_be(client.public_key());
        assert!(public > BigUint::default() && public < *N);
        // Two clients never share an ephemeral.
        assert_ne!(client.public_key(), SrpClient::new().unwrap().public_key());
    }

    #[test]
    fn hostile_server_values_are_refused() {
        let client = SrpClient::from_private(&[7u8; 32]);
        let key = [1u8; 32];
        let salt = [2u8; 16];
        let cases: [(&str, Vec<u8>, &[u8]); 5] = [
            ("empty B", Vec::new(), &salt),
            ("B wider than N", vec![1u8; N_LEN + 1], &salt),
            ("B = 0", vec![0u8; 8], &salt),
            ("B = N", N_BYTES.to_vec(), &salt),
            ("empty salt", vec![5u8; 32], &[]),
        ];
        for (name, b, salt) in cases {
            let err = client
                .process_challenge("dev@example.com", &key, salt, &b)
                .unwrap_err();
            assert!(matches!(err, AppleSignInError::Srp { .. }), "{name}: {err}");
        }
        // An exactly N-wide B that is not 0 mod N is fine.
        let mut b = vec![0u8; N_LEN];
        b[N_LEN - 1] = 3;
        assert!(client
            .process_challenge("dev@example.com", &key, &salt, &b)
            .is_ok());
    }

    #[test]
    fn derivation_bounds_are_checked_before_any_work() {
        let password = Password::new("pw".to_string());
        for (salt, iterations) in [
            (&b""[..], 1000),
            (&[0u8; MAX_SALT_LEN + 1][..], 1000),
            (&b"salt"[..], 0),
            (&b"salt"[..], MAX_ITERATIONS + 1),
        ] {
            assert!(matches!(
                derive_password_key(&password, salt, iterations, PasswordProtocol::S2k),
                Err(AppleSignInError::Srp { .. })
            ));
        }
        assert!(matches!(
            derive_password_key(
                &Password::new(String::new()),
                b"salt",
                1,
                PasswordProtocol::S2k
            ),
            Err(AppleSignInError::Srp { .. })
        ));
        // The bounds themselves are accepted.
        assert!(
            derive_password_key(&password, &[1u8; MAX_SALT_LEN], 1, PasswordProtocol::S2k).is_ok()
        );
    }

    #[test]
    fn protocols_parse_like_spaceship() {
        assert_eq!(
            PasswordProtocol::from_apple("s2k").unwrap(),
            PasswordProtocol::S2k
        );
        assert_eq!(
            PasswordProtocol::from_apple("s2k_fo").unwrap(),
            PasswordProtocol::S2kFo
        );
        assert!(matches!(
            PasswordProtocol::from_apple("unsupported"),
            Err(AppleSignInError::Srp { message }) if message.contains("unsupported")
        ));
        assert_eq!(PasswordProtocol::OFFERED, ["s2k", "s2k_fo"]);
    }

    #[test]
    fn secrets_never_show_in_debug() {
        let password = Password::from("hunter2-secret".to_string());
        assert_eq!(format!("{password:?}"), "Password([redacted])");
        let proof = SrpProof {
            m1: vec![1; 32],
            m2: vec![2; 32],
        };
        assert_eq!(format!("{proof:?}"), "SrpProof([redacted])");
        assert_eq!(
            format!("{:?}", SrpClient::from_private(&[9u8; 8])),
            "SrpClient([redacted])"
        );
    }
}
