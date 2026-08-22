// The telemetry column: which of a machine's readings become rows, and what
// each one says.
//
// `sysGauges` is the one answer to "what does a machine's telemetry look like"
// — WORK's SYSTEM rail and HOME's COMPUTE column both draw what it returns, and
// they had already drifted apart by a `°` back when each spelled it out for
// itself. So the assertions here are about the *list*, not about either page.
//
// The disks are why this file exists. The daemon has published `SysDto.disks`
// since it learned to read the mount table and no client drew one, so these
// pin the two decisions a client has to make about them: which mounts are worth
// a row, and what a row of a filesystem reads like. Both have a counterpart in
// `crates/butai-client/src/chrome/mod.rs` — `disk_mounts` and `cap_pair` — and
// the point of writing them down twice is that the two clients agree.

import { describe, expect, test } from "bun:test";

import { railDisks, sysGauges } from "../src/pages/parts.ts";
import type { DiskDto, DiskKind, SysDto } from "../src/protocol/generated/protocol.ts";

const disk = (
  mount: string,
  used_gb: number,
  total_gb: number,
  extra: Partial<DiskDto> = {},
): DiskDto => ({
  mount,
  source: "/dev/" + mount.replace(/\//g, "-"),
  fstype: "ext4",
  kind: "local" as DiskKind,
  used_gb,
  total_gb,
  stale: false,
  ...extra,
});

const sys = (disks: DiskDto[]): SysDto =>
  ({
    cpu_pct: 12,
    cpu_temp: null,
    cpu_hist: [],
    cpu_model: null,
    cpu_cores: null,
    cpu_threads: null,
    ram_used_gb: 8,
    ram_total_gb: 32,
    ram_hist: [],
    swap_used_gb: 0,
    swap_total_gb: 0,
    gpus: [],
    net: [],
    disks,
    containers: [],
    stacks: [],
  }) as unknown as SysDto;

describe("railDisks", () => {
  // A workstation's mount table is mostly tmpfs and a docker host's is mostly
  // image layers, and neither is a disk that can fill.
  test("draws the real disks and not the plumbing", () => {
    const s = sys([
      disk("/media/archive", 3300, 3667),
      disk("/media/fast", 853, 916),
      disk("/", 191, 215),
      disk("/dev/shm", 0, 39, { kind: "memory" as DiskKind }),
      disk("/mnt/nas", 4, 8, { kind: "network" as DiskKind }),
      disk("/home", 1, 2),
      ...Array.from({ length: 30 }, (_, i) =>
        disk(`/snap/thing${i}`, 0.2, 0.2, { kind: "layer" as DiskKind }),
      ),
    ]);
    // Largest first is the daemon's own order, and it is the order to cut from:
    // `/home` is the fourth real disk, so the cap is doing work here.
    expect(railDisks(s).map((d) => d.mount)).toEqual(["/media/archive", "/media/fast", "/"]);
  });

  test("a machine with no telemetry has no disks", () => {
    expect(railDisks(null)).toEqual([]);
    expect(railDisks(undefined)).toEqual([]);
  });
});

describe("sysGauges", () => {
  test("a disk is one row, naming its mount and its capacity", () => {
    const rows = sysGauges(sys([disk("/media/fast", 898.7, 915.8), disk("/", 202, 215.4)]));
    // cpu, ram, then a row per disk — the gpus are absent, not empty rows.
    expect(rows.map((r) => r.key)).toEqual(["cpu", "ram", "disk:/media/fast", "disk:/"]);
    expect(rows[2]).toMatchObject({ label: "dsk /media/fast", text: "899/916G" });
    expect(rows[3]).toMatchObject({ label: "dsk /", text: "202/215G" });
    // The bar is the fullness, which is not what the readout says.
    expect(Math.round(rows[2]!.value)).toBe(98);
    expect(rows[2]!.tone).toBe("bad");
  });

  // Four digits nobody reads, against eight cells that say the same thing —
  // and in the binary units `df -h` prints, because `df` is where anyone will
  // go to check the number.
  test("a terabyte disk is drawn in terabytes", () => {
    const rows = sysGauges(sys([disk("/media/archive", 3564.4, 3667.4)]));
    expect(rows[2]!.text).toBe("3.5/3.6T");
  });

  // A mount that missed the daemon's sweep keeps its last reading. The row has
  // to carry which of the two facts it is: 99% full and a minute out of date is
  // news about the clock, not an alarm about the disk.
  test("a stale mount says so and is not drawn as an alarm", () => {
    const rows = sysGauges(sys([disk("/mnt/nas-backup", 99, 100, { stale: true })]));
    expect(rows[2]!.label).toBe("dsk /mnt/nas-backup (stale)");
    expect(rows[2]!.text).toBe("99/100G");
    expect(rows[2]!.tone).not.toBe("bad");
    expect(rows[2]!.tone).toBe("accent");
  });

  // A daemon reporting a disk of size zero is a division by nothing, and the
  // page must not go blank over it.
  test("a disk with no capacity is a row at zero, not a NaN", () => {
    const rows = sysGauges(sys([disk("/mnt/empty", 0, 0)]));
    expect(rows[2]!.value).toBe(0);
    expect(rows[2]!.text).toBe("0/0G");
  });
});
