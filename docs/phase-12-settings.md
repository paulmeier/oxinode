# Phase 12 — changing settings from the panel

Where phase 12 stands: **built, held to golden images, and running on the
board; the pad walk through it has not been done from this desk.** The Radio
screen's menu opens an editor for each of the five parameters a host sets, a
confirmed value goes through the same validation a host's configuration does,
and the two-controller question the phase had to answer first is answered
below, from Reticulum's interface code rather than from first principles.

Read-only screens are useful. The request was to change settings from the
board with no app attached, and this is that.

## The two-controller question

An RNode is host-controlled. Reticulum sets the radio up when it brings the
interface online and believes what it set. If the panel changes the frequency
underneath a running `rnsd`, the host's idea of the radio and the radio
disagree. The issue offered three options and asked that the choice be made
by reading `RNodeInterface.py`, so here is what it does, from RNS 1.5.0:

* **It validates exactly once.** `configure_device` runs `initRadio`, which
  sends the five setters and `CMD_RADIO_STATE` on, then calls
  `validateRadioState` — and that is the only call to it in the file. It
  sleeps a quarter of a second, compares `r_frequency`, `r_bandwidth`,
  `r_txpower`, `r_sf` and `r_state` against what it asked for, and if they
  match sets `online`. Nothing runs it again.
* **It does read unsolicited parameter frames, and does nothing with them.**
  `readLoop` updates `r_frequency` and the rest whenever a `CMD_FREQUENCY`
  or similar arrives, logs the value at `LOG_DEBUG`, and — for `CMD_SF` and
  `CMD_CR` — recomputes its bitrate figure. That is all. It goes on
  transmitting on the channel it configured, with `self.frequency` unchanged,
  and the next packet it sends carries no notice that anything moved. A
  `CMD_RADIO_STATE` of zero is one debug line: *"Radio reporting state is
  offline"*.
* **The only frame that makes it look again is an error.** `CMD_ERROR` with
  `ERROR_INITRADIO` raises an `IOError` out of `readLoop`, which takes the
  interface offline and, five seconds later, `reconnect_port` reopens the
  port and calls `configure_device` — which sends the host's own five
  setters again. So a device that wanted the host to re-check would have to
  take the link down to do it, and what comes back up is the host's
  configuration, over the top of the panel's.

That settles the options. **Option 2, edit freely and tell the host, does not
exist**: the frames the modem can send are either ignored past a debug line
or revert the edit. That leaves the panel refusing to touch what a host owns,
and the question is only *what* a host owns.

**The decision is option 3: edit only what the host does not own.** For the
five radio parameters — every field this phase's editors change — the host
owns them all, so for this phase the rule and option 1 coincide:

> **A live session on USB or Bluetooth owns the live radio configuration.**
> While one is there, an action that would change the radio — an edit, the
> toggle, a reset — opens a notice saying which host has it and how to get it
> back, and does nothing else. With nobody on the line, the panel is a
> controller.

"A live session" is DTR asserted on the KISS port, or a phone connected —
the same test the Home screen's `Host` row already makes. It is deliberately
not "the host has turned the radio on": `rnodeconf` holds the port open
without touching the radio, and a host in the middle of `initRadio` has the
port open and the radio still off, and both of those are hosts that would be
surprised. The stricter test is the one that is never wrong about who is in
charge; its cost is that a terminal left open on the KISS port locks the
panel, which the notice says in one line.

The rule is one function, `Action::changes_the_radio`, applied in one place
in the firmware, and the simulator's scene applies the same function so the
golden images of the notice are the board's behaviour and not an
impersonation of it. What it leaves the panel free to do under a host is what
the host does not own: `Save Config`, which stores the live configuration as
the boot one and changes nothing the host reads; `Forget Phones`; the screen
and the restarts. Display brightness and Bluetooth on/off are also the
panel's by this rule, and are not built here because nothing on the panel
edits them yet; when something does, this is the rule it follows.

## What was built

**A third navigation level.** The shell stopped at two on purpose and left
room for the exception, and `Nav` now has it: an `Overlay` that is nothing, a
menu, an editor, or a notice. Up and down change the value, left and right
move the cursor where there is one, select confirms, back cancels. Sideways
inside an editor moves a digit, not the screen. The screen's scroll is kept
while an editor is over it, so a Radio screen scrolled to the sync word is
still there when the editor closes. Any key closes a notice.

The navigator does not open an editor itself. A menu item hands back
`Action::Edit(field)` and the caller answers with `Nav::edit` or
`Nav::notice`, because opening one needs the configuration to edit and the
navigator is not handed that — and because the caller is the one that knows
whether a host has the line. The firmware's `perform` does it in six lines;
the simulator's `Scene::press` does it in the same six.

**The value model, in `oxinode_core::edit`.** An `Editor` opens on a copy of
the whole configuration and a field. The four fields with a fixed set are
steppers: up is the next member above the candidate, down the next below,
saturating at the ends rather than wrapping, because wrapping from 22 dBm to
−17 on one more press is the kind of surprise that ends a field test. A
value that is not in the set — a bandwidth a host set — steps onto the set
in the direction pressed rather than sticking. The frequency is a digit
editor: six digits, `MMM.kkk`, a cursor under one of them, and whatever the
original had below a kilohertz is kept unseen, because no channel plan is
drawn in hertz.

**Refused, not clamped.** Confirming puts the candidate into the copy and
asks `ValidConfig::new` — the same function that decides whether a host's
configuration may reach the chip. A candidate that fails stays in the editor
with the reason under it, unapplied; stepping again clears the refusal. The
sets are deliberately wider than what the radio accepts so that this path is
reachable: power runs to 22 dBm because that is what an RNode host can ask
for, and bandwidth lists the ten LoRa bandwidths a host offers, six of which
this chip does not have. A stepper that only ever produced legal values would
never show the refusal, and the first time it mattered would be a value
nobody had tested. Spreading factor and coding rate walk the RNode range,
all of which this chip does; there is no refusal to reach there and the test
says so rather than pretending.

**Cancel leaves the previous value in place**, and the test for it is the
whole argument for the design: the editor holds a copy, confirm is the only
thing that hands a value out, and cancel hands out nothing. The
configuration the caller holds was never given to the editor by reference,
so there is nothing to restore. `cancel_returns_nothing_and_closes` in the
navigator and `cancelling_leaves_the_original_untouched` in the editor pin
it, and the simulator's `a_cancelled_edit_leaves_the_state_alone` pins it
end to end through a script.

**The protocol's side.** `Protocol::set_from_panel` is the host's setter
without the reply: the setting lands unvalidated, as a host's does, and if
the radio is on it is reprogrammed through `ValidConfig` or refused with the
reason in `last_error` and the radio off — `a_panel_edit_is_refused_the_way_a_hosts_is`
drives a host and a panel to the same impossible power and asserts the two
`Protocol`s end in the same state. The host's radio lock holds against the
panel too. `toggle_from_panel`, `save_from_panel` and `reset_from_panel` are
`CMD_RADIO_STATE`, `CMD_CONF_SAVE` and "back to what the board boots with".
None of it answers on the wire, because by the rule nobody is on the wire
when it happens; a host that connects afterwards configures the board as it
would any other.

`Radio On/Off`, `Reset Config` and `Forget Phones` — logged and not done
since phase 11, pending this decision — are done now. The first two are
radio actions and get the notice under a host. Forgetting phones clears the
stored bonds and raises a signal the Bluetooth task answers between
connections by removing every bond the host stack holds; a phone connected
at the time keeps its session and is forgotten when it goes.

## Persistence

The issue said settings that should survive a reboot go in the device record
at `0xEA000`, and that the record grew from version 1 to 2 without breaking
version 1. It did not need to grow for this. The stored radio configuration
already has a home the host defined: the `CONF_*` bytes of the EEPROM image,
which `rnodeconf --tnc` writes and the record has carried since phase 6.

So the rule is the same one that decides who may edit:

* **In TNC mode** — a stored configuration exists — a panel edit goes into
  the stored configuration as well as the live one, because the stored one
  is what the board boots with, and an edit that vanished at the next power
  cycle would be a setting that only looked changed.
  `in_tnc_mode_a_panel_edit_survives_a_reboot` edits, encodes the record,
  decodes it into a fresh `Protocol`, resumes, and finds the new frequency.
  Reflashing does not touch the record, for phase 6's reason.
* **Under host control** nothing is stored. The host owns that configuration
  and sets it again on every connect, so a stored copy of a panel edit would
  be a boot configuration nobody asked for. A person who wants a
  host-controlled board to boot on what they just set has `Save Config`,
  which is `CMD_CONF_SAVE` from the panel and makes the board a TNC.

The record stays at version 2.

## What the pictures show

Eighteen new golden images, generated from the field list rather than written
out, so a field added to `edit::Field` is a missing image on the next run:

* every editor open on a standalone board, `radio-edit-{frequency, bandwidth,
  spread-factor, coding-rate, tx-power}`;
* the three refusals that can be reached, `-refused` — 815 MHz, 41.7 kHz and
  21 dBm — with the reason wrapped under the value;
* the frequency editor with its cursor moved and a digit changed;
* the Radio screen after a confirmed edit, reading `18 dBm` where the fixture
  had 17;
* the notice, once for USB over an edit and once for a phone over the toggle;
* and the six new menu items, one image each, as every menu item gets.

The first render of the editor found two things the pixel tests had not. The
hint line read `Back: cance`: it was twenty-one characters in a twenty-wide
row, and `Text` truncates silently — right on the board, where a line one
character too long should lose that character rather than vanish, and wrong
in a test, where the loss is the bug. `Lines` now records that a line was
cut, and every screen, editor and notice asserts it was not. The cursor was
drawn as the font's missing-glyph box, because the font has no `^`; it is an
underscore now, which the font does have and which reads as a cursor under a
digit.

The simulator gained a `standalone` fixture — the populated board with
nobody on the line and TNC mode on — because the populated one has a host on
USB, and under that fixture every editor is the notice. Its `tty` mode shows
what is being edited and whether it was refused on the status line.

## On the board

The image was flashed over the 1200-baud touch and read back: it boots, loads
the record, brings the radio up, and answers a host. What has not been done
is a walk through the editors with the pad, which needs a person at the
board; the board's own log says what every press does (`ui: editing
frequency`, `ui: set frequency; stored=true`, `ui: frequency refused, a host
has the radio (USB has the radio.)`), so that walk will be a log to read as
well as a screen to look at.

## What it does not do

* Display brightness and Bluetooth on/off are the panel's by the rule above
  and have no editor yet.
* Nothing edits the fields a host cannot set — preamble, sync word, CRC,
  header mode, IQ — for the same reason phase 4's console cycles them: a
  host could not read them back, and a board whose sync word differs from
  what its host believes is a board that hears nothing and cannot say why.
* `Reset Config` in host mode goes to phase 4's default, not to "whatever
  the last host set", because the last host's configuration is not kept
  anywhere once a panel edit has replaced it.

## Done when

- [x] **The two-controller question is decided, and the decision is written
      down with its reasoning.** Above: option 3, from `RNodeInterface.py`
      validating once and reverting on the only frame that makes it look
      again; for the radio parameters that is a live session owning the
      live configuration.
- [x] **Every radio parameter is editable, validated identically to the host
      path, refused not clamped.** Five editors; `ValidConfig::new` on
      confirm and again in `set_from_panel`; the refusal kept with its
      reason; `a_panel_edit_is_refused_the_way_a_hosts_is`.
- [x] **Cancel leaves the previous value in place, verified by test.**
      `cancelling_leaves_the_original_untouched`,
      `cancel_returns_nothing_and_closes`,
      `a_cancelled_edit_leaves_the_state_alone`.
- [x] **Settings that should persist do, across a reboot and across a
      reflash.** In TNC mode, through the stored configuration in the
      record; `in_tnc_mode_a_panel_edit_survives_a_reboot`, and the record
      is the one a reflash cannot reach.
- [x] **Golden images cover the editor, including a refused value.**
      Eighteen images; three of them refusals.
