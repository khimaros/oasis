# grapheneos: wifi goes dead after joining a captive portal

seen on 2026-10-05 with a pixel 8 pro (husky), android 17, grapheneos build
2026100201, while joining the `OASIS` network of an ESP32.

## symptom

wifi stops working for every network. the setting still reads "on", the
list of networks stays empty, and scans fail. the switch in settings does
not bring it back.

## trigger

all three together:

- the phone is connected to another wifi network
- the network being joined is a captive portal, as every oasis is (R4)
- wifi calling is registered over wifi

joining with wifi off or disconnected does not run into it.

## who starts the switch

it happened five times on 2026-10-05, and the log of four was read
(09:50, 10:04, 10:05, 12:17). in each of those the phone began to move to `OASIS`
by itself, about two seconds after the wifi settings were opened and with
no connect request in the log: its network selection prefers the network
that was picked by hand last. the tap on `OASIS` arrived about three
seconds later, as a connect request from settings, while that automatic
switch was under way. a join that was not interrupted like this, started
over adb with nothing else connected, went through every time.

## what the device sees

the last occurrence was logged on the ESP32 too, and the two logs agree
to the millisecond. the device saw two stations, since the phone uses a
new random address per interface: the second interface joined, got
`10.0.0.2`, and left after 2.6 s. then the first joined, got `10.0.0.3`,
and left after 1.1 s. both left on their own (reason 8). the device
answered dhcp and the connectivity probe as for any other client.

## what logcat shows

the times are those of the first of two occurrences that day. the second,
at 10:04, ran through the same steps with the same intervals.

1. `09:50:44` android keeps `wlan0` on the old network and joins `OASIS` on a
   second interface, `wlan1` ("make before break")
2. `09:50:46` the connectivity probe is redirected, and
   `WifiMbbManager: onCaptivePortalDetected: stopping current primary CMM`
   schedules a stop of `wlan0`. wifi calling defers it by 4000 ms
3. `09:50:47` settings sends another connect request for `OASIS`. android
   tears down `wlan1` and retries on `wlan0`, which is about to stop. the
   attempt fails within half a second
4. `09:50:50` the deferred stop runs: `wlan0` goes, `wpa_supplicant` is
   terminated, the wifi HAL stops, `WifiController` enters `DisabledState`
5. after that every scan logs `WifiService: Failed to start scan`, while
   `cmd wifi status` still answers "Wifi is enabled"

the device does nothing out of the ordinary here. this is a race in
android's wifi module.

## recovery without a reboot

with the phone on usb, `make phone-wifi`, which runs:

    adb shell cmd wifi set-wifi-enabled enabled

`ADB` names another adb binary, and adb itself takes the phone and the
server from `ANDROID_SERIAL` and `ANDROID_ADB_SERVER_PORT`:

    ANDROID_SERIAL=38261FDJG001LY ANDROID_ADB_SERVER_PORT=5039 make phone-wifi

the log then shows `Starting primary ClientModeManager` and the phone is
back on its old network a few seconds later. this worked every time it
happened that day, five times, the first time with a `disabled` sent
before it.

the stop of `wlan0` does not always wait the full 4000 ms: in the last
occurrence it ran after 3.1 s, when wifi calling moved to the mobile
network.

## avoiding it

- open the wifi settings and do not tap `OASIS`: wait for the phone to
  move there by itself. not tried yet
- or turn off auto-connect for `OASIS` on the phone, so that only the tap
  starts a join. not tried yet
- turn wifi off and on, or leave the current network, before joining
- or turn wifi calling off while testing

## what an oasis could do about it

the path starts with the phone learning that the network is a captive
portal while it still holds its old network. an oasis that answered the
probe of android with success would not be seen as a portal, and the
phone would finish the switch the ordinary way. the price is R4: android
would no longer open the portal by itself. not tried.

## upstream

no report names this cause. the closest on the grapheneos issue tracker:

- [#5861](https://github.com/GrapheneOS/os-issue-tracker/issues/5861)
  pixel 8 pro, wifi turns off and will not scan until a reboot. open
- [#8810](https://github.com/GrapheneOS/os-issue-tracker/issues/8810)
  pixel 8 pro on 2026092501, no networks detected. closed as not planned
- [#8877](https://github.com/GrapheneOS/os-issue-tracker/issues/8877) and
  [#8819](https://github.com/GrapheneOS/os-issue-tracker/issues/8819)
  WPA3 networks stopped connecting on the last two builds
