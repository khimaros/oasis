"""what `fcad check` cannot know about this case: that the openings are
open, that the lid closes over the lip, and that the board has room.

run it with `fcad test`.
"""

import FreeCAD as App

from fcad import testing

V = App.Vector


def main():
    c = testing.Checks()
    mod = testing.load(testing.find())
    p = mod.PARAMS
    d = mod._derive(p)
    parts = testing.specs(mod)
    tray = testing.solids(mod, parts["tray"])[0]
    lid = testing.solids(mod, parts["lid"])[0]
    board = testing.solids(mod, parts["board"])[0]
    inside = lambda solid, x, y, z: solid.isInside(V(x, y, z), 1e-6, True)
    mid_wall = -p["wall"] / 2.0
    far_wall = d["len"] + p["wall"] / 2.0
    usb_z = sum(d["slot"]) / 2.0

    usb_y = p["usb_offset"]
    c("the usb slot goes through its wall", not inside(tray, mid_wall, usb_y, usb_z))
    c("the wall stands beside the usb slot", inside(tray, mid_wall, d["half"], usb_z))
    c("the wall is closed in front of the serial socket", inside(tray, mid_wall, -usb_y, usb_z))
    c("the wall bridges the usb slot",
      inside(tray, mid_wall, usb_y, d["slot"][1] + d["bridge"] / 2.0))
    c("the antenna hole goes through its wall",
      not inside(tray, far_wall, 0, d["shoulder"] / 2.0))
    c("the antenna wall is whole around the hole",
      inside(tray, far_wall, p["antenna_hole"], d["shoulder"] / 2.0))
    c("the tray is hollow down to the floor", not inside(tray, 1.0, 0, 0.1))
    c("the floor is closed", inside(tray, 1.0, 0, -p["floor"] / 2.0))

    joint_z = d["shoulder"] + p["lip"] / 2.0
    outside = d["half"] + p["wall"]
    c("the lip is the inner half of the wall",
      inside(tray, 10, d["half"] + p["wall"] / 4.0, joint_z)
      and not inside(tray, 10, outside - p["wall"] / 4.0, joint_z))
    c("the rim of the lid goes around the lip",
      inside(lid, 10, outside - p["wall"] / 8.0, joint_z)
      and inside(lid, 10, -outside + p["wall"] / 8.0, joint_z))
    c("the lid covers the tray and its brow the usb slot (%.1f)" % lid.BoundBox.XMin,
      testing.near(lid.BoundBox.XMin, -p["wall"] - p["brow"])
      and testing.near(lid.BoundBox.ZMax, d["ceiling"] + p["lid"]))
    c("the lid sits on the lip without cutting into the tray (%.3f mm^3)"
      % testing.overlap(tray, lid), testing.overlap(tray, lid) < testing.TOUCH_VOL)
    c("the lid touches the tray (%.3f)" % tray.distToShape(lid)[0],
      tray.distToShape(lid)[0] < 1e-6)

    c("the board collides with nothing",
      testing.overlap(tray, board) < testing.TOUCH_VOL
      and testing.overlap(lid, board) < testing.TOUCH_VOL)
    c("the lid holds the board down within %.1f mm (%.2f)" % (p["hold"], lid.distToShape(board)[0]),
      testing.near(lid.distToShape(board)[0], p["hold"]))
    c("the board stands on the floor", testing.near(board.BoundBox.ZMin, 0.0))
    stop_x = p["pcb_len"] + 2 * p["slack"] + mod.STOP_LEN / 2.0
    c("stops stand behind both corners of the pcb",
      all(inside(tray, stop_x, side * (d["half"] - 1.0), d["pcb_top"] - 0.5) for side in (1, -1)))
    c("the stops leave the module free",
      not inside(tray, stop_x, 0, d["pcb_top"] - 0.5))
    box = tray.BoundBox
    c("the tray is %.1f x %.1f x %.1f mm" % (box.XLength, box.YLength, box.ZLength),
      box.XLength < 100 and box.YLength < 40)
    return c.report()


testing.main(main, __file__)
