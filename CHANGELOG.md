# Changelog

Derived from [conventional commits](https://www.conventionalcommits.org).

## Unreleased

### Features

- Add an opt-in macOS toggle to send system shortcuts such as `Command+Space` to the
  remote desktop while its picture has focus. Press `Shift+Escape` to release the keyboard.

## v0.4.0 (2026-11-23)
### Documentation
- Tighten the changelog sections since v0.2.0 ([9e7d0bf](https://github.com/teomurgi/mac-gliff/commit/9e7d0bf0bbb84af721f23c84e01b7eee0e752c0b))
- Lead the README with the core selling points ([1c9d388](https://github.com/teomurgi/mac-gliff/commit/1c9d38886904f990bf4f55836e21f7a65b133fa6))
- Install from GitHub Releases as a normal Mac app ([f37be41](https://github.com/teomurgi/mac-gliff/commit/f37be41d5e04858fb0b1c99aea6d8ed3f11545a6))

### Features
- Play the remote machine's sound (#3) ([dfa16b7](https://github.com/teomurgi/mac-gliff/commit/dfa16b7759b5de4de5830c7d26e194c1e41d8dbd))
- Sync client with upstream gliff 0.4.3 (#4) ([f29f258](https://github.com/teomurgi/mac-gliff/commit/f29f2584a05e07f1afff602bec60ccce6dca6f83))

## v0.3.0 (2026-10-17)
### Features
- Ask how to map the remote screen; hide local pointer on headless (#2) ([f69497c](https://github.com/teomurgi/mac-gliff/commit/f69497c5e143d8b5a0e887948028d34f1afa9e28))

## v0.2.1 (2026-10-17)
### Documentation
- The release dmg is Developer ID signed and notarized ([24c5f86](https://github.com/teomurgi/mac-gliff/commit/24c5f868a04395223733d9bac19ad1353281bc6d))
- Lead the README with a live-session screenshot ([d19a560](https://github.com/teomurgi/mac-gliff/commit/d19a5606a77676f72675e3278a81ea8ad13028c1))
- Point the README screenshot at .github/assets ([da5f1a9](https://github.com/teomurgi/mac-gliff/commit/da5f1a9214a0d559203f955bfd2370b5e16633f1))
- Swap in the dusk-cityscape desktop screenshot ([542da12](https://github.com/teomurgi/mac-gliff/commit/542da1205c655fb6d8d4e83a6edcb5b8a1ade8dc))

### Fixes
- Map the U key ([73ad3cf](https://github.com/teomurgi/mac-gliff/commit/73ad3cf48a94f3a541c0151fbb0af287d6cf4b8a))

## v0.2.0 (2026-10-17)
### Build
- Sign and notarize the release dmg with a Developer ID ([74e2037](https://github.com/teomurgi/mac-gliff/commit/74e20377964e6bcee53c622dde380ec3efada6e8))

### Documentation
- The DMG stays unsigned while waiting on the Apple developer subscription ([31ccf50](https://github.com/teomurgi/mac-gliff/commit/31ccf501e47dac490878776af55896b8f50b3f9a))
- Refresh signing status and upstream credits ([066fdaa](https://github.com/teomurgi/mac-gliff/commit/066fdaafd1756c45e5cf13206796ba2cadc7e854))

### Features
- Ship keymaps for every Apple keyboard layout with an xkb counterpart ([e6fb2b2](https://github.com/teomurgi/mac-gliff/commit/e6fb2b222622630998f6388886b48217d8458407))
- Ship the bundle as Gliff.app ([07a3a7e](https://github.com/teomurgi/mac-gliff/commit/07a3a7ec13d00dd665290e422edb1e0172dc2252))

## v0.1.1 (2026-10-11)
### Fixes
- Resolve ssh helpers from the GUI launch PATH ([e59f9c6](https://github.com/teomurgi/mac-gliff/commit/e59f9c67b527a6c5655e8e2828dce5ed5984536e))

## v0.1.0 (2026-10-11)
### Documentation
- README, contributing guide and license for the public launch ([8ef5160](https://github.com/teomurgi/mac-gliff/commit/8ef51605f16efafb9f6e37443a300dad9f2208e1))

### Internal
- Run the macOS tests under lldb for a backtrace ([313c0d2](https://github.com/teomurgi/mac-gliff/commit/313c0d270aabeb16c947c404712ebc9e87466b5e))
- Fix the lldb invocation (executable filter, process launch args) ([12c95eb](https://github.com/teomurgi/mac-gliff/commit/12c95eb7a71fa66695b8d86d23d44a257d0eefc9))
- A C probe of FFmpeg's exact VT flow over the fixture ([b420ef2](https://github.com/teomurgi/mac-gliff/commit/b420ef28d7df7d838fb9742f85e0b1a08f373742))
- Umbrella framework headers for the probe ([86e1e8c](https://github.com/teomurgi/mac-gliff/commit/86e1e8c7a78f432bb26d80af4162bbc59b73b257))
- Enforce conventional commits and automate changelog and releases ([faad3d2](https://github.com/teomurgi/mac-gliff/commit/faad3d2e6c62181af08719c41512e4d6c648af7e))
- Start public versioning at 0.1.0 ([0f4bb42](https://github.com/teomurgi/mac-gliff/commit/0f4bb42379d99b12063034f87bf180ec15c9ac4e))

### Other
- Vendor gliff 0.3.1 (omacom/gliff @ a2edd50) ([ba02503](https://github.com/teomurgi/mac-gliff/commit/ba025039f6a3330d1950ad72c97f874e5db95bf0))
- The macOS GPU tier — VideoToolbox decode + Metal recombine ([6075c02](https://github.com/teomurgi/mac-gliff/commit/6075c02fa1f0ab7d1a56ace9a49ac370928a2da7))
- Do not link a Dispatch framework ([510b753](https://github.com/teomurgi/mac-gliff/commit/510b75320a71a0812deb2161753a27906acd5848))
- Release the semaphore with dispatch_release ([1eb2d4a](https://github.com/teomurgi/mac-gliff/commit/1eb2d4a258838fe7ae06e99e696873be748a810f))
- Vt, compute: fixes from the first real macOS run ([ca4a4ed](https://github.com/teomurgi/mac-gliff/commit/ca4a4edc64a667073de30858b093ea09c2ed27b5))
- Complete the CMBlockBufferCreateWithMemoryBlock signature ([bd5f824](https://github.com/teomurgi/mac-gliff/commit/bd5f824eb20b9ba9c2891db9d010ab6fb77748c1))
- Logical-RGBA writes and grouped dispatch ([5567cc7](https://github.com/teomurgi/mac-gliff/commit/5567cc7f3b157f599819b087aedeea08b4fe5857))
- Reference the caller's sample buffer, FFmpeg's flags ([0af8c23](https://github.com/teomurgi/mac-gliff/commit/0af8c2395311eac2b3563e4502b2b59403580214))
- CMBlockBufferCreateWithMemoryBlock has no sourceBuffer ([9005986](https://github.com/teomurgi/mac-gliff/commit/90059863e9fd51fdf3708d375239815d8721849f))
- Progress markers around the decode pipeline ([9a28714](https://github.com/teomurgi/mac-gliff/commit/9a28714c19cb2f98151681843f8e3c31bb20d644))
- A software-decode constructor for virtual machines ([99a45c1](https://github.com/teomurgi/mac-gliff/commit/99a45c10d27c75c0e16c18dbb51b32db65b85475))
- VTDecompressionSessionDecodeFrame takes infoFlagsOut ([cf82b7c](https://github.com/teomurgi/mac-gliff/commit/cf82b7c296bb03b90312797293f3e508562c96d2))
- Clean up after the runner debugging ([e2edfb9](https://github.com/teomurgi/mac-gliff/commit/e2edfb9c5603c0384f06a5c13968858056c363a7))
- Add handoff notes for resuming on a real Mac ([6160b73](https://github.com/teomurgi/mac-gliff/commit/6160b739f3f5ea83472a29435eea5a8b216510fa))
- Wire the VideoToolbox+Metal tier into the client ([531d7f6](https://github.com/teomurgi/mac-gliff/commit/531d7f64f0e44f6d8b160d6bbf2c6b349402cb3b))
- IOSurface-backed recombine outputs ([1a95b71](https://github.com/teomurgi/mac-gliff/commit/1a95b71b0dbf99f3cc76bf9e593cdce3260b5cab))
- Zero-copy display — the GLArea video widget ([28e24ed](https://github.com/teomurgi/mac-gliff/commit/28e24ed44a20cfba97db518d0f96bde122a91a45))
- Input — the kVK→evdev table and shipped keymaps on macOS ([543e266](https://github.com/teomurgi/mac-gliff/commit/543e2667b4d04f54b9928d9f6dac1e43806bdd27))
- Fix the macOS clipboard deadlock and the keymap send ([438640f](https://github.com/teomurgi/mac-gliff/commit/438640fce9a7f10682c8ab8d5bc3747540c6d7c5))
- Ship the client as mac-gliff ([f33bc13](https://github.com/teomurgi/mac-gliff/commit/f33bc1396b7f188e38f0e3823939af698ea82f6b))
- As_chunks for the constant-size chunk loops ([d310362](https://github.com/teomurgi/mac-gliff/commit/d310362ee3670de73ee485c9c85c634979d44e32))
- Keep the glvideo example compiling on Linux ([28577fe](https://github.com/teomurgi/mac-gliff/commit/28577fe5a80c74b654615a220967f750d37a1f76))
- App icon and a DMG bundle ([a236c73](https://github.com/teomurgi/mac-gliff/commit/a236c7344a8991eb3f144c787bde8df8cda876fb))
- Fully relocate pixbuf loaders and bake bundle-relative cache ([b8e9adc](https://github.com/teomurgi/mac-gliff/commit/b8e9adc24afacd83b8d4e03eeaad45da7375ba10))
- Gitignore macOS .DS_Store ([fca70ba](https://github.com/teomurgi/mac-gliff/commit/fca70ba4a823e1939e0dfce6731f6511badf82ca))
- Local ⌘ shortcuts and a launcher chord for the captured keyboard ([b93cda0](https://github.com/teomurgi/mac-gliff/commit/b93cda0dd9cd213bfe8696f4954c996a83a36a02))
- MacOS traffic-light window buttons ([ac73eef](https://github.com/teomurgi/mac-gliff/commit/ac73eef520875a4b92b7d2203130e6a9af28eb4a))
- Pin the Rust toolchain to 1.99.0 ([cc378cd](https://github.com/teomurgi/mac-gliff/commit/cc378cd3239f0b88cf1affaacf8227680bfd682e))
- Make the workspace macOS-only ([d862d3b](https://github.com/teomurgi/mac-gliff/commit/d862d3b60531c88abcc92eb1ab8f45f63a696cbd))
- Carry rustfmt and clippy in the pinned toolchain ([9c0456d](https://github.com/teomurgi/mac-gliff/commit/9c0456db129e76cb0568987af67a69cc3d48b903))


