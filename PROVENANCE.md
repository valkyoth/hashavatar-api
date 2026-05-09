# Provenance

This repository is intended to be a code-generated avatar system.

## Asset statement

- No raster sprite sheets are bundled with the crate.
- No third-party character packs are bundled with the crate.
- No licensed avatar art is embedded in the crate as source assets.
- Avatar output is drawn procedurally from Rust code using geometric primitives.

## Generation model

- Identity input is hashed with `SHA-512`.
- Digest bytes are mapped into visual parameters such as proportions, colors, spacing, and facial details.
- Final images are rendered using drawing primitives provided by Rust crates such as `image` and `imageproc`.

## Current avatar families

- 23 families including Cat, Dog, Robot, Fox, Alien, Monster, Ghost, Slime, Bird, Wizard, Skull, Paws, Planet, Rocket, Mushroom, Cactus, Frog, Panda, Cupcake, Pizza, Ice Cream, Octopus, Knight.

## Background modes

- Themed
- White

## Practical implication

The repository is materially different from avatar systems that depend on pre-made asset packs. The visuals are generated from code at runtime, which avoids the usual licensing concerns around bundled image libraries. This file is not legal advice; it is a technical provenance statement about how the repository is constructed.

## Output formats

- WebP
- PNG
- JPEG
- GIF
- SVG

- Lossless WebP
- PNG
- SVG
