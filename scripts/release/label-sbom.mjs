import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

const path = resolve(process.argv[2] ?? "");
const version = process.argv[3];
if (!path || !version) throw new Error("Usage: label-sbom.mjs <spdx-json> <version>");

const sbom = JSON.parse(readFileSync(path, "utf8"));
if (
  !String(sbom.spdxVersion ?? "").startsWith("SPDX-") ||
  sbom.SPDXID !== "SPDXRef-DOCUMENT" ||
  !Array.isArray(sbom.packages) ||
  sbom.packages.length === 0 ||
  !Array.isArray(sbom.relationships) ||
  sbom.relationships.length === 0 ||
  sbom.relationships.some(
    (relationship) =>
      !relationship.spdxElementId ||
      !relationship.relationshipType ||
      !relationship.relatedSpdxElement,
  )
) {
  throw new Error("Generated SPDX document is missing required document or dependency structure");
}

sbom.name = `XiaoLi-v${version}-source-dependency-sbom`;
sbom.creationInfo ??= {};
sbom.creationInfo.comment =
  "Dependency inventory generated from the immutable release source workspace; it is not a binary provenance attestation.";
writeFileSync(path, `${JSON.stringify(sbom, null, 2)}\n`, "utf8");
console.log(`Labeled source dependency SBOM (${sbom.packages.length} packages): ${path}`);
