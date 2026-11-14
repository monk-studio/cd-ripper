# CDR

## Reference sources

Other rippers' source, for studying how they solve the same problems. Read
for ideas only: XLD's files say "All rights reserved", so copy no code.

- **XLD 20250302** (build 157.2): `~/oss/sourceforge/xld`. The same version
  as the XLD.app we benchmark against. Ripping is in `XLD/XLDCDDABackend.c`
  (drive I/O), `XLD/XLDSecureRipperEngine.m` (secure mode) and
  `XLD/XLDController.m` (offset list lookup).
- **XLD, GitHub mirror** (`gmw/xld`): `~/oss/github/gmw/xld`. Stuck in 2018;
  don't use it for how XLD behaves today.
- **CUETools** (CTDB client): `~/oss/github/gchudov/cuetools.net`. The CTDB
  lookup protocol is in `CUETools.CTDB/CUEToolsDB.cs`.
