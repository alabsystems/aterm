// Copyright 2026 Andrew Yates
// Test vector from Proton AG proton-srp; see LICENSE-Proton-MIT.
// SPDX-License-Identifier: MIT
use super::*;
#[allow(
    dead_code,
    reason = "Retain all fields of the published authentication fixture"
)]
struct SrpInstance {
    version: u64,
    username: &'static str,
    password: &'static str,
    modulus: &'static str,
    salt: &'static str,
    server_ephemeral: &'static str,
    client_secret: &'static str,
    expected_client_ephemeral: &'static str,
    expected_client_proof: &'static str,
    expected_server_proof: &'static str,
}
const VECTOR: SrpInstance = SrpInstance {
    version: 4,
    username: "test",
    password: "test",
    modulus: "G2TfKd7dhlYkXbfu51FEKtnPHa/FpxqUB2OFwvv5+nrWPpTLNl7JTrpb4THPY9OTDKxHVd5tBiXCTdmpBlUdIWYBIi66lP9Qx4uLJtvydjb0AZ8XALoJEodGLP+tT4iyLWa7+JkwkIZeRtB37PHbeMsqsNA2rXhrBGtdk71HPJV3mRTLk/YH/X77nTQWGVEmPOeUvxgfswHuRE0XCZnq/5QnzEFRvZGnVfGhhACcFBixbux7/C1fiNQrOKTMF2tz6rEy/jfdfhFi3KHRPoGm8Q8JDed+uMxJLNCxm7b8FR9bStVrFDCNWC2GRxOQxCI0AK7j1elMlz+5l9Wfip8wnw==",
    salt: "Jl54BOeNTVl8Ng==",
    server_ephemeral: "ycjIyRFPVgILQUczPERQnD0txE5jmJEIjXZa3G6lIDi6XELRuQtHIHVCOQ2iUHg4EaeSHvcXqa29o50n6mR1wZ6P9zduWG3ww2ThxTMvWvLlI4s3lzZVXlL+ncaEk3D6okjb1qHszAP+pm2ZdxUhHSCZE5IHHWTCXwlxOaxvNuYzpCTyW/DK1XgRM8ysrWHC/JLhFpTW/CfBQi0d0XRWmVb+1SvdSHR4MOj24FQLrNA0hbayRYEp7wQbL7Ts+I8lOB/w7E8KiYe2+DXcUdTozGdMPGOsf9n/w7ZULtXXH03a7hfl74sZ4caCbk0RBswq4Mj8y5kpMGXadnby5oHShQ==",
    client_secret: "eOu9ioj1jqrnee1w1HJ8Op9L7LLcCtME40q2HEAAhu4=", // Published Proton test vector; gitleaks:allow
    expected_client_ephemeral: "1HLx1rlk4H/0no233yNKLxdcPd+IfvyLF1c6R5ZzCFOKy8XprU1APUWpm9Q+A5hu5HlSaVlcUBj1xS3TNT487OAa2bvCS0ryDfTOax2ZtVGGQ4i+O5e5OgO6MV7ORx97DpwzU4N6t6D9hdUByeH0yWAXe+6OVLPcouMu0x487qDvIbivXJVqMzaP8yGMTGeZWwj+03d4ShPzXQEdAADqDWJcs9ktQOUE1feioN3c6eGTeMhUf2RKDjS8GtEqj927Hk8wBPAIlWWd0S3rCibuimpU3giDxy/cHOHFoT35yS1DFYoQNytqwZelcdCi+tcUznlbO7HD3tl+M9nlqpeLjg==",
    expected_client_proof: "+7Ocq5542c9zfVWMSkXeG/I7mcz5DIrg0dMu7NmraD6J7+2zyBWqIjlc4Ej+ZiP7CNUBjTeEvjwZ+xLIMsJorbOWhjkyq9S4PkARw1b8IsIajfloUV9vlLqmxP9bKrJ7Xk6KQ9pMziqf4qA1O6dW55s7H3Git4zKZlxQLjW/sQnABaYtyfzGgCC2hQUIYJiAH1tZVNezcyYtUHICpFUwj3t4afw0+pbunIdDjuf1YOWixreupLfLgml1IMXBm7fkZYIrnnO5aEskrprRJpJDg2iSSqhxguOnsHbnC+wVjXDZtap7Am8mRh4b/Hv0iWCqCkTf1YeHqYJCuqcbmVCw+A==",
    expected_server_proof: "D9Oj+Qiju2+H/xqGwpDXa4ceSogtyo4sBgKoirTHnIJSL8jRZL+dNqvhG1FuiOlMk9K75tfS7umBLCGAyTC1RsNS5vDE1U3Vrkg29XI4P4q4hjf3NxVq0F/NPrYNcuyJLTSXBHr0T+8d79WmM1UGTQsw/UILuGURDkqouSKFSADIEuv4QYQ21KxcIep+ptLQy/0oio15ciFGC4w6lnT+wLCHp6HoBcteRrz0bnlAfdoSSWZiL91MkYCU7++wV4q8VVp7HwIBNGYvLE9nnGSvOuBMFhsB8HgpxO8EQcVl/plQiZk5/cYQCRsiOP6XqxyDFgQpXPcQwz1FVWd8dycatA==",
};

#[test]
fn matches_proton_client_and_server_proof_vector() {
    let modulus = aterm_codec::base64::decode(VECTOR.modulus).unwrap();
    let secret = aterm_codec::base64::decode(VECTOR.client_secret).unwrap();
    let proof = calculate(
        VECTOR.password,
        VECTOR.version,
        VECTOR.salt,
        &modulus,
        VECTOR.server_ephemeral,
        Some(&secret),
    )
    .unwrap();
    assert_eq!(proof.client_ephemeral, VECTOR.expected_client_ephemeral);
    assert_eq!(proof.client_proof, VECTOR.expected_client_proof);
    proof.verify_server(VECTOR.expected_server_proof).unwrap();
    assert!(proof.verify_server(VECTOR.expected_client_proof).is_err());
}
