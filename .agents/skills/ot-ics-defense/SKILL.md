---
name: ot-ics-defense
description: Monitor, evaluate, respond to, and prevent cyber attacks on Operational Technology (OT), Industrial Control Systems (ICS), PLCs, RTUs, and SCADA/HMI infrastructure.
---

# OT / ICS Critical Infrastructure Defense (`ot-ics-defense`)

Use this skill when operating inside `isa-centra-spike` (`centra-actual`, `orange-watch`, `greyfox-recon`, `timberwolf-red`) or `cyber-lab` to assess, monitor, harden, or verify infrastructure configurations involving water/wastewater facilities, electric grid substations, pipelines, building management systems (BMS), or manufacturing automation.

## 1. Four-Stage Operational Workflow

1. **Monitor (`orange-watch` — Threat & Advisory Intake):**
   - Check CISA Known Exploited Vulnerabilities (KEV) and ICS-CERT advisories (`www.cisa.gov`, `nvd.nist.gov`) for active exploitation of PLC/RTU firmware, SCADA/HMI web portals, industrial cellular routers, and VPN appliances.
   - Read `references/controller-protocol-matrix.md` via `read_skill` to map vendor hardware/software to affected protocols and ports.
2. **Evaluate (`greyfox-recon` — Purdue Surface & Exposure Audit):**
   - Run `node scripts/verify/audit-ot-exposure.mjs <target-path>` against infrastructure manifests (`docker-compose.yml`, firewall configs, gateway configs, `.env`, or JSON/YAML deployment specs).
   - Identify three critical failure classes responsible for >85% of real-world OT breaches:
     - **Class A (Internet/0.0.0.0 Exposed Industrial & Remote-Screen Ports):** Modbus (`:502`), S7comm/IEC-61850 (`:102`), EtherNet/IP (`:44818` / `:2222`), DNP3 (`:20000`), IEC-104 (`:2404`), BACnet (`:47808`), OPC-UA (`:4840`), Unitronics PCOM (`:20256`), CODESYS (`:2455`), TriStation (`:1502`), Niagara Fox (`:1911` / `:4911`), VNC (`:5900`), RDP (`:3389`), Ignition (`:8088` / `:8043`).
     - **Class B (Default Vendor Credentials & Unauthenticated Write Paths):** Factory PINs (`1111`), `admin/admin`, `anonymous` OPC-UA write sessions, or remote desktop sessions without hardware MFA.
     - **Class C (Flat IT-to-OT Bridging & Missing Setpoint Clamps):** Missing Purdue Level 3.5 DMZ, `network_mode: "host"` on OT workloads, or missing software/hardware min-max clamps on physical actuators (chemical dosing, pump VFD frequency, valve states).
3. **Respond (`timberwolf-red` — Containment & Verified Countermeasures):**
   - Isolate affected interfaces by rebinding public `0.0.0.0` ports to loopback (`127.0.0.1`) or encrypted private tunnel interfaces (`100.64.x.x`).
   - Enforce read-only telemetry splits (virtual data diode) so external dashboards can query status (`GET`) without possessing network paths or credentials to issue actuator writes (`POST`).
   - Add deterministic min/max physical setpoint clamps and bearer/mTLS authentication checks, then prove the fix with an automated test (`audit-ot-exposure.mjs` + unit tests).
4. **Prevent (`centra-actual` — Continuous 5-Gate Receipt):**
   - Require a passing `audit-ot-exposure.mjs` receipt and zero exposed OT/management ports before approving any deployment or closing a mission.
