# alsa-sys

Rust raw FFI bindings for ALSA.

To avoid too long build times, the finished binding is committed into this
repository. If you would prefer to generate the bindings at build time, there
is a `use-bindings` feature to do so.

## Installing build dependencies

### Debian/Ubuntu/Mint

```
sudo apt install libasound2-dev
```

### Fedora/Centos

```
dnf install alsa-lib-devel
```

## Regenerating bindings

To regenerate the bindings yourself, run `regenerate_bindings.sh`. This
will generate the bindings in the build script, and run the
`regenerate_bindings` binary, which copies the generated bindings into
`src/`.
