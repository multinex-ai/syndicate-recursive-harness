#!/usr/bin/env node
/**
 * OT / ICS Critical Infrastructure Exposure & Configuration Auditor (`audit-ot-exposure.mjs`).
 *
 * Used by `greyfox-recon` and `timberwolf-red` to audit Docker Compose, IaC, firewall,
 * gateway, and controller manifests against CISA Cross-Sector Performance Goals (CPGs)
 * and Purdue Model Level 0-3.5 isolation rules.
 */
import { readdir, readFile, stat } from "node:fs/promises";
import path from "node:path";

export const PROTECTED_OT_PORTS = Object.freeze({
  102: "Siemens S7comm / IEC 61850 MMS",
  502: "Schneider / Generic Modbus TCP",
  1217: "CODESYS V2 Runtime",
  1502: "Schneider Triconex TriStation (SIS)",
  1911: "Tridium Niagara Fox",
  2222: "Rockwell EtherNet/IP Implicit UDP",
  2404: "IEC 60870-5-104 Substation SCADA",
  2455: "CODESYS V3 Runtime",
  3389: "Remote Desktop Protocol (RDP)",
  4840: "OPC-UA Server",
  4911: "Tridium Niagara Fox TLS",
  5900: "Virtual Network Computing (VNC / HMI Screen)",
  8088: "Inductive Automation Ignition Gateway HTTP",
  20000: "DNP3 Substation / Water Telemetry",
  20256: "Unitronics PCOM PLC+HMI (CISA AA23-335A)",
  44818: "Rockwell / Allen-Bradley EtherNet/IP CIP",
  47808: "BACnet/IP Building & Chiller Automation",
});

const DEFAULT_CREDENTIAL_PATTERNS = [
  { id: "OT-CRED-001", rule: "Unitronics / PLC default PIN '1111'", regex: /(?:^|[_.\s-])(?:pin|pass(?:word)?|auth(?:_token)?)\s*[:=]\s*["']?1111["']?(?:\s|$)/i },
  { id: "OT-CRED-002", rule: "Default vendor admin password", regex: /(?:^|[_.\s-])(?:pass(?:word)?|secret)\s*[:=]\s*["']?(?:admin|admin123|password|default|0000)["']?(?:\s|$)/i },
  { id: "OT-CRED-003", rule: "Anonymous OPC-UA / Modbus write enabled", regex: /(?:^|[_.\s-])(?:anonymous_write|allow_anonymous_write|unauthenticated_write)\s*[:=]\s*true\b/i },
];

export function auditConfigText(relativePath, text) {
  const findings = [];
  const lines = text.split(/\r?\n/);

  for (let idx = 0; idx < lines.length; idx += 1) {
    const line = lines[idx];
    const lineNo = idx + 1;

    // 1. Check for public 0.0.0.0 or bare wildcard port bindings on protected OT/HMI ports
    for (const [portStr, protocolName] of Object.entries(PROTECTED_OT_PORTS)) {
      const port = Number(portStr);
      const publicBindRegex = new RegExp(
        `(?:0\\.0\\.0\\.0:${port}\\b|["']${port}:${port}(?:\\/(?:tcp|udp))?["']|\\bhost_port\\s*[:=]\\s*${port}\\b)`,
        "i",
      );
      const loopbackRegex = new RegExp(`(?:127\\.0\\.0\\.1|100\\.64\\.\\d+\\.\\d+):${port}\\b`);
      if (publicBindRegex.test(line) && !loopbackRegex.test(line)) {
        findings.push({
          id: "OT-PORT-001",
          severity: "critical",
          file: relativePath,
          line: lineNo,
          port,
          protocol: protocolName,
          message: `Public binding on protected OT/HMI port ${port} (${protocolName}); bind to 127.0.0.1 or encrypted private tunnel IP (100.64.x.x).`,
        });
      }
    }

    // 2. Check for factory default PLC / HMI credentials
    for (const pat of DEFAULT_CREDENTIAL_PATTERNS) {
      if (pat.regex.test(line)) {
        findings.push({
          id: pat.id,
          severity: "critical",
          file: relativePath,
          line: lineNo,
          message: `${pat.rule} detected; rotate factory credentials and enforce MFA/mTLS before commissioning.`,
        });
      }
    }

    // 3. Check for unsegmented host networking or remote PLC programming mode
    if (/\bnetwork_mode\s*:\s*["']?host["']?\b/i.test(line)) {
      findings.push({
        id: "OT-SEG-001",
        severity: "high",
        file: relativePath,
        line: lineNo,
        message: "Container uses network_mode: host, bypassing Purdue Level 3.5 network segmentation.",
      });
    }
    if (/(?:^|[_.\s-])plc_mode\s*[:=]\s*["']?(?:REM|REMOTE_PROG|PROGRAM)["']?(?:\s|$)/i.test(line)) {
      findings.push({
        id: "OT-PLC-001",
        severity: "high",
        file: relativePath,
        line: lineNo,
        message: "PLC keyswitch/mode configured for REMOTE_PROG; lock operational controllers in RUN mode.",
      });
    }
  }

  return findings;
}

export async function auditPath(targetPath) {
  const resolved = path.resolve(targetPath);
  const info = await stat(resolved);
  const files = [];
  const eligible = new Set([".yml", ".yaml", ".json", ".toml", ".conf", ".env", ".ini"]);

  async function walk(current) {
    for (const entry of await readdir(current, { withFileTypes: true })) {
      if ([".git", "target", "node_modules", "runtime", "models"].includes(entry.name)) continue;
      const full = path.join(current, entry.name);
      if (entry.isDirectory()) await walk(full);
      else if (eligible.has(path.extname(entry.name)) || entry.name.startsWith(".env")) files.push(full);
    }
  }

  if (info.isDirectory()) await walk(resolved);
  else files.push(resolved);

  const findings = [];
  for (const file of files) {
    const rel = path.relative(resolved, file) || path.basename(file);
    const text = await readFile(file, "utf8");
    findings.push(...auditConfigText(rel, text));
  }

  return {
    schema_version: 1,
    scanned_files: files.length,
    passed: findings.length === 0,
    findings,
  };
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const target = process.argv[2] ?? ".";
  const report = await auditPath(target);
  console.log(JSON.stringify(report, null, 2));
  if (!report.passed) process.exit(1);
}
