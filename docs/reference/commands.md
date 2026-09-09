# RNode commands

The command set as `oxinode_core::rnode::command` decodes it, and what the
protocol does with each. Command bytes are those of Reticulum's
`RNodeInterface.KISS` and `rnodeconf.KISS` (RNS 1.5.0). "Raw" in the escaping
column means the host reads that field straight out of the byte stream with
no un-escaping, so the device must not escape it.

## Radio

| Byte | Command | Payload | Escaped by host | Handling |
|---|---|---|---|---|
| `0x00` | `DATA` | packet | yes | Transmit (after carrier sense, with the air header, split at 254). Inbound: a received packet, preceded by `STAT_RSSI` and `STAT_SNR`. |
| `0x01` | `FREQUENCY` | u32 Hz | yes | Set; echoed back. Reports the *wanted* frequency, not the commanded one. |
| `0x02` | `BANDWIDTH` | u32 Hz | yes | Set; echoed back. |
| `0x03` | `TXPOWER` | i8 dBm | raw | Set; echoed back. |
| `0x04` | `SF` | u8 | raw | Set; echoed back. |
| `0x05` | `CR` | u8, denominator of 4/n | raw | Set; echoed back. |
| `0x06` | `RADIO_STATE` | `0x00` off, `0x01` on, `0xFF` ask | raw | Applies the configuration through `ValidConfig`, or refuses; reports the resulting state. |
| `0x07` | `RADIO_LOCK` | u8 | raw | Set; reported. Holds against panel edits too. |
| `0x0B` | `ST_ALOCK` | u16, hundredths of a percent | yes | Accepted and reported. |
| `0x0C` | `LT_ALOCK` | u16 | yes | Accepted and reported. |
| `0x0F` | `READY` | — | — | Outbound: sent after every transmission; releases a host running with flow control. |

## Identification

| Byte | Command | Payload | Handling |
|---|---|---|---|
| `0x08` | `DETECT` | `0x73` | Answered with `0x46`. Any other payload is malformed. |
| `0x0A` | `LEAVE` | — | The host is closing the interface. |
| `0x47` | `BOARD` | — | `0x32`, homebrew. |
| `0x48` | `PLATFORM` | — | `0x70`, nRF52. This is what makes the host assume a display. |
| `0x49` | `MCU` | — | `0x71`, nRF52. |
| `0x50` | `FW_VERSION` | — | `1.52`: the protocol version spoken, not oxinode's release. Below 1.52 the host panics. |

## Statistics

| Byte | Command | Direction | Escaped by host | Notes |
|---|---|---|---|---|
| `0x21` | `STAT_RX` | out | yes | packets received since boot |
| `0x22` | `STAT_TX` | out | yes | packets transmitted since boot |
| `0x23` | `STAT_RSSI` | out | raw | last packet's RSSI, offset by 157 |
| `0x24` | `STAT_SNR` | out | raw | last packet's SNR in quarter-dB, signed; −16 dB is clamped because `0xC0` would end the frame |
| `0x25` | `STAT_CHTM` | out | yes | channel and airtime statistics |
| `0x26` | `STAT_PHYPRM` | out | yes | physical-layer parameters |

## Provisioning

| Byte | Command | Payload | Handling |
|---|---|---|---|
| `0x51` | `ROM_READ` | — | The whole 256-byte EEPROM image in one frame. |
| `0x52` | `ROM_WRITE` | `[addr, value]` | Written to the RAM copy; committed 250 ms after the last write. |
| `0x59` | `ROM_WIPE` | `0xF8` | Erases the image. Requires the confirm byte. |
| `0x53` | `CONF_SAVE` | — | Stores the current radio configuration and enters TNC mode. |
| `0x54` | `CONF_DELETE` | — | Forgets the stored configuration. |
| `0x55` | `RESET` | `0xF8` | Commits the record and resets. Requires the confirm byte. |
| `0x56` | `DEV_HASH` | — | SHA-256 over the identity block and the MCU's device ID. Answered only when provisioned. |
| `0x57` | `DEV_SIG` | 64 bytes | Stored in the record. |
| `0x58` | `FW_HASH` | 32 bytes | The target firmware hash, stored. |
| `0x60` | `HASHES` | `0x01` target / `0x02` running | `0x01` answered from the record; `0x02` not answered (an image cannot hash itself). |
| `0x61` | `FW_UPD` | — | Accepted. |

## Display

| Byte | Command | Payload | Handling |
|---|---|---|---|
| `0x41` | `FB_EXT` | u8 on/off | Show the host's picture (doubled to 128 × 128) instead of the device's own page. |
| `0x43` | `FB_WRITE` | `[line, 8 bytes]` | One row of the 64 × 64 external framebuffer. |
| `0x42` | `FB_READ` | — | The 512-byte framebuffer back. |
| `0x66` | `DISP_READ` | — | The screen, folded to 128 × 64 by OR-ing row pairs, 1024 bytes page-major. |
| `0x45` | `DISP_INT` | u8 | Display contrast. |
| `0x63` | `DISP_ADR` | u8 | **Not applicable**: the address is discovered by scanning the bus. |
| `0x64` `0x67` `0x68` | `DISP_BLNK` `DISP_ROT` `DISP_RCND` | | Known, not implemented. |
| `0x30` | `BLINK` | | Known, not implemented. |

## Bluetooth

| Byte | Command | Handling |
|---|---|---|
| `0x46` | `BT_CTRL` | Known, not implemented. Bluetooth is always on; pairing is initiated from the phone. |
| `0x62` | `BT_PIN` | Known, not implemented. The passkey is shown on the panel. |

## Not on this board

`0x65` `NP_INT` (neopixel), `0x69` `DIS_IA`, and the WiFi set `0x6A`–`0x6E`,
`0x84`, `0x85` (`WIFI_MODE`, `WIFI_SSID`, `WIFI_PSK`, `CFG_READ`,
`WIFI_CHN`, `WIFI_IP`, `WIFI_NM`) are reported as **not applicable**: there is
no hardware for them, and a log line that said "not yet" about one would be
telling somebody to wait for something that is not coming.

## Errors

`0x90` `ERROR` is outbound only. The first two codes make the host raise an
`IOError` and drop the interface; the rest it records and carries on.

| Code | | Host reaction |
|---|---|---|
| `0x01` | `INITRADIO` | drops the interface |
| `0x02` | `TXFAILED` | drops the interface |
| `0x03` | `EEPROM_LOCKED` | records it |
| `0x04` | `QUEUE_FULL` | records it |
| `0x05` | `MEMORY_LOW` | records it |
| `0x06` | `MODEM_TIMEOUT` | records it |

A refused radio configuration is *not* reported as `INITRADIO`: the radio
state comes back off, the host finds the mismatch itself, and the specific
reason goes to the log port. See [The RNode protocol](../architecture/rnode-protocol.md).
