# Vendored protocol definitions

These `.proto` files are copied verbatim from the
[flipperdevices/flipperzero-protobuf](https://github.com/flipperdevices/flipperzero-protobuf)
repository, which is distributed under the GPL-3.0-or-later licence.  They
describe the wire protocol implemented by the Flipper Zero firmware.

The files are vendored rather than fetched at build time so that builds are
reproducible and work offline.  Update them with:

```sh
for f in flipper application desktop gpio gui property storage system; do
    curl -sSO "https://raw.githubusercontent.com/flipperdevices/flipperzero-protobuf/master/$f.proto"
done
```
