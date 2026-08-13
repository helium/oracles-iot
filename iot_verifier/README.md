# IoT Verifier

Proof of Coverage was retired on IoT (HIP-0149), so this service now has two tasks:

1.  Verify incoming packet reports. Takes data-transfer packet reports as input
    from an S3 bucket and classifies each as rewardable or non-rewardable
    (a gateway must be known to iot-config to be rewardable).
2.  Calculate & distribute rewards. Aggregates rewardable data-transfer shares
    and distributes the daily emissions, writing reward shares and a manifest to S3.

Beacon and witness ingest endpoints remain live (gateways still send them) but
the reports are discarded without processing.

## Reward allocation

Of each epoch's oracle-emitted rewards:

| Bucket        | Share                                              |
|:--------------|:---------------------------------------------------|
| Data Transfer | 50% (capped; unused portion flows to Operations)   |
| Operations    | 37% + any Data Transfer underflow                  |
| Oracles       | 7%                                                 |

## S3 Inputs

| File Type      | Source                        |
|:---------------|:------------------------------|
| IotValidPacket | Helium Packet Router / ingest |
| PriceReport    | price oracle                  |

## S3 Outputs

| File Type           |
|:--------------------|
| NonRewardablePacket |
| IotRewardShare      |
| RewardManifest      |

## Catching up after downtime

The verifier is configured for continuous operation. If it has been down long
enough that the incoming packet window has aged out, confirm
`loader_window_max_lookback_age` exceeds the outage plus a small buffer so the
packet loader does not skip past unprocessed files.
