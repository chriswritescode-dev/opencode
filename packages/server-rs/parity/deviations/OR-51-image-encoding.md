# OR-51: re-encoded and resized images are not byte-identical to photon

TypeScript: `packages/core/src/image/photon-normalize.ts:62-148` normalises tool image parts with
`@silvia-odwyer/photon-node@0.3.4`: it decodes the base64 image, keeps an image that fits unless the PNG
pass-through rules apply, re-encodes a fitting opaque image as JPEG quality 80 only when that is smaller, and
otherwise walks a 0.75 resize ladder with Lanczos3 and tries PNG or JPEG 80/70/55/40 in the order the transparency
and 65 536-pixel rules choose.

Rust: `crates/tool/src/image.rs` ports the same decisions with the `image` crate (`png`, `jpeg`, `gif` and `webp`
decoding, Lanczos3 resizing and its own PNG and JPEG encoders). The limits, the small-PNG pass-through rule, the
64×64 transparency sample, the resize ladder and the encoder order are the same, so a result has the same
dimensions and MIME type as TypeScript's; its bytes differ because photon is WebAssembly and the encoders are
different implementations.

Reason: the Rust server has no WebAssembly photon runtime, and byte-identical encoder output is not required by the
tool contract. `crates/tool/tests/all/image.rs` covers the unchanged, resized, transparent, undecodable and
size-limited decisions, and checks the output dimensions and MIME type. No parity scenario in this task reads a large
image, so the parity block stays empty.

Accepted: pending owner

```parity
```
