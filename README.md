# Helium IoT Oracles [![CI](https://github.com/helium/oracles-iot/actions/workflows/CI.yml/badge.svg)](https://github.com/helium/oracles-iot/actions/workflows/CI.yml)

Oracles for the Helium IoT Network.

> **Note**: This repository was split from the main [helium/oracles](https://github.com/helium/oracles) repository. For Mobile oracles, see [helium/oracles](https://github.com/helium/oracles).

## Architecture

```mermaid
flowchart TD
    DB1[(Foundation owned db populated by helius)]
    IC("`**IOT Config**
        - Provides access to on-chain data
        - Stores pubkeys for remote systems
        - Store orgs and routes used by Helium Packet Router
    `")
    HPR("`**Helium Packet Router**
        - Ingest packets from Hotspots
        - Deliver packets to LNS
    `")
    IPV("`**IOT Packet Verifier**
        - Burns DC for data transfer (on solana)
    `")
    II("`**IOT Ingestor**
        - Beacons
        - Witnesses
        - Long lived grpc streams
    `")
    IV("`**IOT Verifier**
        - Validates all incoming data
        - Calculates rewards at 01:30 UTC
    `")
    IE("`**IOT Entropy**
        - Creates entropy used by gateways and iot-verifier
    `")
    IP("`**IOT Price**
        - Records Pyth price for IOT
    `")
    IRE("`**IOT Reward Index**
        - Writes rewards to foundation db
    `")
    DB2[(Foundation owned db that stores reward totals)]
    S[(Solana)]
    DB1 --> IC
    IC -- gRPC --> HPR
    HPR -- s3 --> IPV
    II -- s3 --> IV
    IPV -- s3 --> IV
    IPV --> S
    IE -- s3 --> IV
    IP <--> S
    IP -- s3 --> IV
    IV -- s3 --> IRE
    IRE --> DB2
```

## Components

### IoT Verifier
PoC (Proof of Coverage) Verifier - validates beacon/witness reports, calculates rewards.

### IoT Config
Configuration APIs for IoT subnetwork - provides access to on-chain data.

### IoT Packet Verifier
Packet verification - burns Data Credits for data transfer on Solana.

### Supporting Services
- **Ingest**: PoC ingest server (IoT mode)
- **Price**: Price oracle for IOT token
- **Reward Index**: Writes rewards to foundation database
- **PoC Entropy**: Creates entropy for gateways and verifier

## Shared Libraries

This repository depends on shared infrastructure libraries from the [oracles](https://github.com/helium/oracles) repository via git dependencies:

- `file-store` and `file-store-oracles`: File-based storage abstractions
- `db-store`: Database storage layer
- `task-manager`: Task scheduling and management
- `custom-tracing`: Tracing utilities
- `poc-metrics`: Metrics collection
- `tls-init`: TLS initialization
- `price-tracker`: Price tracking utilities
- `reward-scheduler`: Reward scheduling
- `solana`: Solana blockchain integration
- `denylist`: Denylist management

## Development

### Building

```bash
cargo build --release
```

### Testing

```bash
cargo test --workspace
```

### Local Development with Shared Libraries

If you need to modify shared libraries during local development, you can use path overrides in the root `Cargo.toml`:

```toml
[patch."https://github.com/helium/oracles"]
file-store = { path = "../oracles/file_store" }
db-store = { path = "../oracles/db_store" }
# Add other libraries as needed
```

Remember to remove these patches before committing.

## Deployment

IoT services are built as Debian packages and deployed via the CI/CD pipeline. Packages are uploaded to packagecloud at `helium/oracles-iot`.

## Multi-Mode Applications

Some applications in this repository (ingest, price, reward_index, poc_entropy) contain code for both IoT and Mobile networks. Mobile-specific code will be pruned in future updates.
