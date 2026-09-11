# Rocker extension registry format (v1)

A registry is a Git repository used as a static HTTPS origin. Rocker does not
clone it or execute any build step: it fetches `index-v1.json` and its detached
signature, verifies both against a key that the user or application has pinned,
then downloads the immutable package selected from that index.

```text
registry/
  index-v1.json
  index-v1.sig
  packages/
    example.summary-1.2.3.rockerext
    example.summary-1.2.3.rockerext.sig
```

`index-v1.sig` and every `signature` field are standard base64-encoded Ed25519
signatures. The index signature is over the exact UTF-8 bytes of
`index-v1.json`; do not reformat the JSON after signing it. A package signature
is over its exact archive bytes.

The package is a ZIP archive with `extension.toml` at its root, plus its entry
script or WASM component and any assets. It must not contain symlinks, absolute
paths, or `..` path components. The current client limits packages to 20 MiB
compressed, 64 MiB uncompressed, and 256 entries.

The index's `package_url` may point to a file under the repository folder or an
immutable GitHub release asset. It must be HTTPS. Prefer immutable release URLs
and never change archive bytes at a published version.

The application pins the registry ID, index URL, signature URL, and Ed25519
public key outside the repository. A registry cannot introduce or rotate its
own trust key through the index.

See [`index-v1.example.json`](index-v1.example.json) for the signed document
shape.
