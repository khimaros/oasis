# hardware: the ESP32-S3 board

the ESP32-S3 target (R37) was brought up on 2026-10-06 on this board.

## the board

an unbranded ESP32-S3-DevKitC-1 clone with an N16R8 module, sold as "ESP32
S3 N16R8 DevKitC-1 Module ... USB C with Antenna + IPEX (2 PCS)":
<https://www.amazon.com/dp/B0FVD5FQ79>

| what         | value                                                    |
|--------------|----------------------------------------------------------|
| module       | marked ESP32-S3-N16R8, WROOM-1 footprint                 |
| chip         | ESP32-S3, revision v0.2, 40 MHz crystal (read by espflash) |
| flash        | 16MB (read by espflash)                                  |
| PSRAM        | 8MB by the listing. the firmware does not use it         |
| antenna      | a trace on the module, and an IPEX (U.FL) socket next to it |
| usb          | two type C sockets, see below                            |
| serial chip  | CH343P, usb id `1a86:55d3`, `/dev/ttyACM0` on linux      |
| headers      | two rows of 22 pins, soldered on as it comes, pins down  |
| other        | RST and BOOT buttons, a WS2812 LED                       |
| size         | 63.3 x 28 mm with the module, the pcb alone 57 mm long   |

of the two usb sockets, the right one, seen from above with the sockets
towards you, is the serial chip, and the one for flashing and for the log:

    BOARD=esp32s3 FLASH_MB=16 PORT=/dev/ttyACM0 make flash-wipe monitor

the left one is wired to the usb pins of the chip itself, by the vendor's
drawing. it was not tried, also not for power alone.

## the antenna

the listing comes with an antenna of 2.4 GHz for each board: a hinged stub
of 10.5 cm with an RP-SMA plug (a thread on the inside and no pin). it
screws onto an RP-SMA jack, which is the end of a short cable with an IPEX
plug for the socket on the module. the jack has a thread of 1/4 inch on
the outside and goes through a hole in the case, held by its nut.

not checked yet: whether the socket on this module is live as it comes.
modules with both a trace and a socket often choose between them with a
small resistor that has to be moved. the same goes for the range with and
without the antenna.

## other vendors

boards of the same description (N16R8, two type C sockets, CH343P, 44 pins,
an antenna socket) are sold under many names. none of these was bought or
measured, and the case below may not fit them:

- <https://www.amazon.com/dp/B0G8HJPF69> three boards with antennas
- <https://www.amazon.com/dp/B0GVSGCC3Z> two boards, "external antenna support"
- <https://www.amazon.com/dp/B0G1S9LP7J> EC Buying, three boards with an antenna socket
- <https://www.amazon.com/dp/B0F8NDRQQK> three boards, no antenna named
- <https://quartzcomponents.com/products/esp32-s3-wroom-n16r8-dual-c-type-usb-development-board-wifi-bluetooth-module>
- <https://www.bdtronics.com/esp32-s3-wroom-1-development-board-with-wifi-and-bluetooth-5-ble-dual-usb-type-c.html>

the original is the ESP32-S3-DevKitC-1 of espressif. its N16R8 version
with a socket in place of the trace carries the ESP32-S3-WROOM-1U module.
it has micro usb sockets on older revisions, so the usb slot of the case
would need another look.

## the case

`case/case.fcad` is a case in two printed parts, a model for
[fcad](https://github.com/khimaros/fcad) and FreeCAD:

    make case       # builds case/dist, checks the model, runs its test

the tray and the lid are printed one by one, each from its own file:
`case/dist/parts/tray.stl` and `case/dist/parts/lid.stl`. both print as
they are, without supports. the lid is modeled plate down.

`make case-gcode` slices both with PrusaSlicer, for an Ender-3 V2 with a
0.4 mm nozzle and PLA, into `case/dist/gcode`: `tray-superdraft.gcode` and
`lid-superdraft.gcode` at 0.28 mm layers (about 1h25 and 45 minutes), and
`tray-normal.gcode` and `lid-normal.gcode` at 0.20 mm (about 1h55 and 55
minutes). `PRINTER`, `FILAMENT`, `PRINT_superdraft`, and `PRINT_normal`
name other presets of the slicer.

`case/dist/case.FCStd` is the assembly for FreeCAD: the tray, the lid, and
a stand-in for the board where it sits. the stand-in is also there by
itself, as `case/dist/parts/board.FCStd` and `board.stl`. it is not for
printing.

- the tray is 94.7 x 33.4 x 21.5 mm. the board goes in with its header
  pins standing on the floor, the usb sockets against the end with the slot
- the slot is 13 x 7 mm, for one plug, in front of the left socket: the
  one of the chip itself, for power. the serial socket stays behind the
  wall, so the board comes out of the case to be flashed over it
- two stops behind the corners of the pcb take the push of a usb plug
- the far end has a hole of 6.6 mm for the RP-SMA jack, and 26 mm of room
  behind it for the jack and its cable
- the lid has a rim that goes around a lip on the tray, so water that
  lands on the lid runs off over the outside of the joint. a brow of the
  lid stands 4 mm out over the usb slot
- two pads under the lid end 0.5 mm over the shield of the module and over
  the usb sockets, which keeps the board on its pins

the lid is held by the fit of rim and lip alone (`fit` in `PARAMS`, 0.2 mm
per side). the usb slot and the thread of the jack are open: this case
keeps off rain and spills from above while it lies flat. it does not
survive being put under water. a bead of silicone in the joint and around
the jack is the way to more.

nothing of this was printed yet. the numbers come from the drawing of the
vendor and from data sheets. measure before printing:

- how far the middle of the left usb socket is from the middle of the pcb
  (`usb_offset`, 7 mm, a guess from the photo), and the grip of your plug
  against the slot (`usb_slot_wid`, `usb_slot_hgt`)
- that the left socket powers the board on its own
- the length of the pins under the pcb (`pin_drop`, 8.7 mm)
- the jack and its cable against the bay (`antenna_bay`)
