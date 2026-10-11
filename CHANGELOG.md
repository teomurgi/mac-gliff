# Changelog

Derived from [conventional commits](https://www.conventionalcommits.org).
## v0.1.0 (2026-10-11)
### Documentation
- README, contributing guide and license for the public launch ([11737c1](https://github.com/teomurgi/mac-gliff/commit/11737c1f646c4a7720dd7c105b7117230e73fdf7))

### Internal
- Run the macOS tests under lldb for a backtrace ([ac4261e](https://github.com/teomurgi/mac-gliff/commit/ac4261ef7f59004373e11038aa7ae059c141b123))
- Fix the lldb invocation (executable filter, process launch args) ([ae7b4ed](https://github.com/teomurgi/mac-gliff/commit/ae7b4edd641a7cccb4355ba52599eac56065cde7))
- A C probe of FFmpeg's exact VT flow over the fixture ([0deb4e4](https://github.com/teomurgi/mac-gliff/commit/0deb4e4627778fbb314719f5634728ce217d7496))
- Umbrella framework headers for the probe ([ba00893](https://github.com/teomurgi/mac-gliff/commit/ba00893401d1cdeb9a362ce77acbb14997344699))
- Enforce conventional commits and automate changelog and releases ([50ca4eb](https://github.com/teomurgi/mac-gliff/commit/50ca4eb010c9f7261cc274b8f4b416c3420d615b))
- Start public versioning at 0.1.0 ([6e47376](https://github.com/teomurgi/mac-gliff/commit/6e47376317d25ff1b808b4ce7216f459eceeb599))

### Other
- Vendor gliff 0.3.1 (omacom/gliff @ a2edd50) ([e919614](https://github.com/teomurgi/mac-gliff/commit/e919614f2705111066cdf178f87918719a963d35))
- The macOS GPU tier — VideoToolbox decode + Metal recombine ([bd5ada9](https://github.com/teomurgi/mac-gliff/commit/bd5ada9eb4d6406b720e5a1e59c68e2ab8fc4ac9))
- Do not link a Dispatch framework ([55aa8ab](https://github.com/teomurgi/mac-gliff/commit/55aa8ab8fbb574b399dcb6d3f5b188e12a70f6ba))
- Release the semaphore with dispatch_release ([007ef5b](https://github.com/teomurgi/mac-gliff/commit/007ef5bcacaa33943bd5cb95604f0ecb3ae7625f))
- Vt, compute: fixes from the first real macOS run ([a028178](https://github.com/teomurgi/mac-gliff/commit/a028178c6ba15bd482b9a8071aca9de55e284064))
- Complete the CMBlockBufferCreateWithMemoryBlock signature ([3847116](https://github.com/teomurgi/mac-gliff/commit/3847116025d7cb671318b9d850c3768979332abd))
- Logical-RGBA writes and grouped dispatch ([49cb07c](https://github.com/teomurgi/mac-gliff/commit/49cb07c198f0cf03a188383ed40d398a69a0e316))
- Reference the caller's sample buffer, FFmpeg's flags ([72e7d57](https://github.com/teomurgi/mac-gliff/commit/72e7d5795532560a2ff65a34d82c2c6caf9c1f2f))
- CMBlockBufferCreateWithMemoryBlock has no sourceBuffer ([9349b74](https://github.com/teomurgi/mac-gliff/commit/9349b74b68857501fbc38a7b6a2f7b59115bdf43))
- Progress markers around the decode pipeline ([67abd35](https://github.com/teomurgi/mac-gliff/commit/67abd356c56202e2639b5391d6132c4e8441eef6))
- A software-decode constructor for virtual machines ([5471f8a](https://github.com/teomurgi/mac-gliff/commit/5471f8a676d797ec788852c6943ac5f06ae5f43f))
- VTDecompressionSessionDecodeFrame takes infoFlagsOut ([7b2a6e2](https://github.com/teomurgi/mac-gliff/commit/7b2a6e232289c215fbcb7ac7148764dbff161e17))
- Clean up after the runner debugging ([87674b9](https://github.com/teomurgi/mac-gliff/commit/87674b9599eb50fb17d573f49e1c360537d320e6))
- Add handoff notes for resuming on a real Mac ([aa4e3dc](https://github.com/teomurgi/mac-gliff/commit/aa4e3dc59fd9ac1ce10b82fce9e7fcb51e9babdd))
- Wire the VideoToolbox+Metal tier into the client ([cd68d4f](https://github.com/teomurgi/mac-gliff/commit/cd68d4f6b8e8eed8fc81203ddace53d3a3984715))
- IOSurface-backed recombine outputs ([49afa31](https://github.com/teomurgi/mac-gliff/commit/49afa31ad30fe90facaf54003bad0513bc2e7904))
- Zero-copy display — the GLArea video widget ([e1f5a11](https://github.com/teomurgi/mac-gliff/commit/e1f5a1182560617276f3876fc25ded6228322615))
- Input — the kVK→evdev table and shipped keymaps on macOS ([a65868b](https://github.com/teomurgi/mac-gliff/commit/a65868b1a7d144d83b602d39655f65756418d1e6))
- Fix the macOS clipboard deadlock and the keymap send ([ee87173](https://github.com/teomurgi/mac-gliff/commit/ee87173a1db7fc6b767b99e2dcf3a2dd533e5ad4))
- Ship the client as mac-gliff ([a0e36b9](https://github.com/teomurgi/mac-gliff/commit/a0e36b9b8b12640fa63453a918b24e6cc4a59502))
- As_chunks for the constant-size chunk loops ([8b5c815](https://github.com/teomurgi/mac-gliff/commit/8b5c815d80be6119252e6e4d79f51183dbeb3bf6))
- Keep the glvideo example compiling on Linux ([99e597d](https://github.com/teomurgi/mac-gliff/commit/99e597d43c58fe10f97707560e43cb54b86129af))
- App icon and a DMG bundle ([ffc6284](https://github.com/teomurgi/mac-gliff/commit/ffc62841faa9b24bac1125e43778be2aaa20e465))
- Fully relocate pixbuf loaders and bake bundle-relative cache ([3de42de](https://github.com/teomurgi/mac-gliff/commit/3de42de694c34d500f36539b0c5c10da4e55a6ca))
- Gitignore macOS .DS_Store ([bcde31e](https://github.com/teomurgi/mac-gliff/commit/bcde31ec5afeadc372d0efb78752e2be1ceb6ccb))
- Local ⌘ shortcuts and a launcher chord for the captured keyboard ([3c1435e](https://github.com/teomurgi/mac-gliff/commit/3c1435eaabd97176690a541be71bb889b6e8b228))
- MacOS traffic-light window buttons ([fac918d](https://github.com/teomurgi/mac-gliff/commit/fac918d701e7c0714d8b56d77b0d9bc1065f3264))
- Pin the Rust toolchain to 1.99.0 ([d3ba32d](https://github.com/teomurgi/mac-gliff/commit/d3ba32dd9d2d023871ae23a76546dc88791b63f8))
- Make the workspace macOS-only ([6997961](https://github.com/teomurgi/mac-gliff/commit/699796123fc2783878880ada79a88625e944f3da))
- Carry rustfmt and clippy in the pinned toolchain ([4fdce2d](https://github.com/teomurgi/mac-gliff/commit/4fdce2dda50550e4a13b19196bc5238af5737db5))


