# OT / ICS Controllers, Software, Protocols & Historical Breach Matrix

## 1. Common Critical Infrastructure Sectors & Physical Machines

| Sector | Physical Machines & Actuators | Primary Process Risks if Manipulated |
|---|---|---|
| **Water & Wastewater** | Chemical dosing metering pumps (sodium hydroxide/lye, chlorine, fluoride), VFD booster pumps, wet-well lift stations, UV/ozone contactors, motorized sluice gates | Chemical over-/under-dosing, storage tank overflow, dry-run pump cavitation, pressure main rupture |
| **Electric Grid & Substations** | Protective relays (IEDs), circuit breakers, load tap changers (LTCs), phase-shifting transformers, synchronous condensers | Uncoordinated breaker trips (blackout), out-of-phase reclosure damage, protection relay blinding |
| **Oil, Gas & Pipelines** | Compressor station turbines, motor-operated block valves (MOVs), custody-transfer flow computers, Safety Instrumented Systems (SIS / ESD) | Over-pressurization, emergency shutdown (ESD) lockouts, custody-transfer telemetry spoofing |
| **Facilities, Data Centers & BMS** | Centrifugal chillers, CRAH/CRAC units, UPS battery switchgear, diesel generator ATS controllers | Thermal runaway / cooling loss in data centers, spoofed fire/damper interlocks |

---

## 2. Common Automation Controllers (PLCs / RTUs / IEDs) & Industrial Protocols

| Vendor | Controller Families | Native Protocol(s) | Default Port(s) | Common Exposure / Weakness |
|---|---|---|---|---|
| **Rockwell Automation / Allen-Bradley** | ControlLogix, CompactLogix, MicroLogix, GuardLogix | EtherNet/IP, CIP (Common Industrial Protocol) | `TCP 44818`, `UDP 2222` | Unauthenticated CIP cpu-stop / program upload when controller keyswitch is left in `REM` (Remote) instead of `RUN` |
| **Siemens** | SIMATIC S7-300, S7-400, S7-1200, S7-1500 | S7comm, S7comm-plus, PROFINET | `TCP 102` | Legacy S7comm (`S7-300/400`) lacks native cryptographic auth; replay/logic injection if port `:102` is reachable |
| **Schneider Electric** | Modicon M340, M580, Quantum; Triconex (SIS) | Modbus TCP, UMAS; TriStation | `TCP 502` (Modbus), `UDP 1502` (TriStation) | Modbus TCP has zero built-in authentication—any host reaching `:502` can read/write coils and holding registers |
| **Unitronics** | Vision Series, UniStream (integrated PLC + HMI) | PCOM (ASCII / Binary) | `TCP 20256` | Frequently deployed on cellular modems in municipal water with factory default admin PIN `1111` (CISA `AA23-335A`) |
| **Schweitzer (SEL) / ABB / GE** | SEL-351/451/751 Relays, ABB RTU500, GE Mark VIe | DNP3, IEC 60870-5-104, IEC 61850 (MMS/GOOSE) | `TCP 20000` (DNP3), `TCP 2404` (IEC-104), `TCP 102` (MMS) | Unauthenticated DNP3 or IEC-104 direct-operate breaker trip commands if substation LAN is bridged to IT |
| **Tridium / Honeywell / Johnson Controls** | Niagara JACE, Metasys, BACnet field controllers | Niagara Fox, BACnet/IP | `TCP 1911/4911` (Fox), `UDP 47808` (BACnet) | Internet-exposed BMS dashboards or UDP `:47808` allowing unauthenticated BACnet `WriteProperty` overrides |
| **Multi-Vendor Embedded OEMs** | WAGO, Beckhoff, Eaton, Festo, Advantech | CODESYS Runtime V2/V3 | `TCP 2455`, `TCP 1217`, `UDP 1740-1743` | Exposed CODESYS runtime ports allowing remote logic download or command execution |

---

## 3. Common SCADA, HMI & Engineering Workstation Software

- **Inductive Automation Ignition (`TCP 8088` HTTP / `8043` HTTPS):** Modern web-based SCADA/MES platform; risk arises when Perspective/Vision gateways are exposed to the public internet without MFA or with permissive OPC-UA write bindings.
- **AVEVA / Wonderware InTouch & System Platform:** Widely used in water and manufacturing; often accessed by operators via RDP (`:3389`), VNC (`:5900`), or TeamViewer.
- **Siemens WinCC & TIA Portal / STEP 7:** Engineering and SCADA suite for SIMATIC PLCs.
- **Rockwell FactoryTalk View & Studio 5000:** Engineering and HMI suite for ControlLogix/CompactLogix.
- **GE / Emerson iFIX & Proficy / DeltaV:** Common in water utilities and power generation.
- **OPC-UA (`TCP 4840`) & Classic OPC DA (DCOM):** Cross-vendor telemetry bus; must disable `Anonymous` login and enforce `Basic256Sha256` / mTLS client certificates with read-only roles for reporting clients.

---

## 4. Historical Patterns of Successful Attacks (MITRE ATT&CK for ICS)

1. **Stuxnet (2010 — Siemens S7-300/400 & STEP 7):**
   - *Pattern:* Compromised engineering workstations via removable media → hooked `s7otbxdx.dll` → injected rogue ladder logic (`OB1`/`OB35`) to spin centrifuges out of bounds while replaying recorded normal RPM telemetry to operator HMIs.
   - *MITRE ICS Tactics:* Modify Program (`T0836`), Spoof Reporting Message (`T0856`), Loss of View (`T0829`).
2. **Ukraine Power Grid — BlackEnergy 3 (2015) & Industroyer / CrashOverride (2016) & Industroyer2 (2022):**
   - *Pattern:* Spear-phishing in corporate IT → harvested VPN credentials → pivoted across insufficiently segmented firewall into SCADA dispatch workstations → used native `IEC-104`, `IEC 61850`, and `OPC DA` modules to open substation breakers + flashed garbage firmware onto serial-to-Ethernet gateways (Moxa) + wiped Windows HMIs (`KillDisk`).
   - *MITRE ICS Tactics:* Valid Accounts (`T0859`), Unauthorized Command Message (`T0855`), Inhibit Response Function (`T0809`).
3. **TRITON / TRISIS (2017 — Schneider Electric Triconex SIS):**
   - *Pattern:* Pivoted from corporate IT into a dual-homed engineering workstation connected to the Safety Instrumented System (SIS) network → used reverse-engineered `TriStation` (`UDP 1502`) protocol to inject payload into Triconex safety controller RAM, causing controllers to enter a fail-safe shutdown.
   - *Lesson:* Safety Instrumented Systems (SIS) must never share engineering workstations or flat L2/L3 networks with process control (BPCS) or corporate IT.
4. **Oldsmar, FL (2021) & Texas Municipal Water Systems (2024):**
   - *Pattern:* Direct internet exposure of operator remote-access software (`TeamViewer`, `VNC :5900`) with shared/default credentials and no MFA → attacker moved cursor on live HMI to increase chemical dosing (NaOH) or toggle pump controls.
5. **INCONTROLLER / PIPEDREAM (2022 — CISA `AA22-103A`):**
   - *Pattern:* Modular state-grade ICS toolkit capable of scanning and interacting directly with Schneider Modicon (`Modbus`/`UMAS`), Omron Sysmac (`FINS`/`HTTP`), CODESYS runtimes, and `OPC-UA` servers.
6. **Aliquippa, PA & Global Unitronics Water PLC Campaign (2023–2024 — CISA `AA23-335A`):**
   - *Pattern:* Shodan sweep for port `TCP 20256` → connected to internet-exposed Unitronics Vision PLC+HMI booster stations still configured with factory default PIN `1111` → halted PLC execution and defaced HMI display.

---

## 5. Mandatory Defensive Architecture (Purdue Model & CISA CPGs)

- **Level 0/1 (Sensors, Actuators, PLCs, RTUs, Safety Interlocks):**
  - Keep physical PLC keyswitches in **`RUN`** (hard-locked against remote program downloads), never `REM` or `PROG`.
  - Enforce hard physical and PLC-level min/max clamps (e.g., max dosing pump duty cycle, high-pH cutoff relay, high-level float switch wired in series) independent of SCADA software.
- **Level 2/3 (HMI, SCADA, Historian, Engineering Workstations):**
  - Zero direct internet routing (`0.0.0.0` bindings forbidden on all industrial and VNC/RDP ports).
  - Rotate all factory defaults (`1111`, `admin`, `anonymous`) before network attachment.
- **Level 3.5 (IT/OT DMZ & Virtual Data Diode):**
  - Replicate historian/telemetry data **outward only** (OT → DMZ) via unidirectional gateway or read-only API; corporate IT (Level 4) must never have an inbound network route to initiate connections to Level 1/2 controllers.
