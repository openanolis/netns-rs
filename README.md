# netns-rs

The netns-rs crate provides an ultra-simple interface for handling
network namespaces in Rust. Changing namespaces requires elevated
privileges, so in most cases this code needs to be run as root.

This crate only supports linux kernel.

## Build

```
cargo build
```

## Test(as root)
```
CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER="sudo -E" cargo test
```
or
```
sudo -E cargo test
```

## Credits
The main resource so far has been the source code of [netns(golang)](https://github.com/vishvananda/netlink), [CNI network plugins](https://github.com/containernetworking/plugins/blob/master/pkg/testutils/netns_linux.go) and [iproute2](https://wiki.linuxfoundation.org/networking/iproute2).

## Altnernatives
[https://github.com/little-dude/netlink](https://github.com/little-dude/netlink): `rtnetlink/src/ns.rs` provides the same functionality, but its creation of netns in a new process feels a bit heavy.

## License

This code is licensed under [Apache-2.0](LICENSE).
