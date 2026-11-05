# pumpkin-java-multiversion

A multi-version Java Edition protocol translation plugin for [Pumpkin](https://github.com/Pumpkin-MC/Pumpkin) using the Pumpkin WASM plugin API.

## Overview

Pumpkin natively targets the latest Minecraft Java protocol (currently 26.3). This plugin adds backward compatibility for older Minecraft Java Edition clients (1.7.2 through 26.2) by:
- Intercepting and translating packet IDs and network payloads.
- Remapping block states, item IDs, sound IDs, entity types, particles, and more across protocol versions.
- Adapting chunk data formats for older clients (1.7, 1.8, 1.9, 1.18).
- Providing seamless multi-version connectivity via Pumpkin's WASM plugin system.

## Supported Versions

- **1.7.2 - 1.7.10** (Protocols 4, 5)
- **1.8.x** (Protocol 47)
- **1.9.x - 1.12.2** (Combat Update through World of Color)
- **1.13.x - 1.15.2** (The Flattening, Village & Pillage, Buzzy Bees)
- **1.16.x - 1.20.4** (Nether, Caves & Cliffs, Wild, Trails & Tales)
- **1.20.5 - 1.21.11** (Armored Paws, Tricky Trials)
- **26.1 - 26.3**


