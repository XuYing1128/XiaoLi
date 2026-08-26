# 小狸 XiaoLi Asset Provenance

## Scope

This record covers the six reviewed image masters reused by XiaoLi v0.2.0-beta.1:

- `src/assets/mochi-green.png`
- `src/assets/mochi-yellow.png`
- `src/assets/mochi-red.png`
- `src/assets/mochi-gray.png`
- `src/assets/mochi-app-icon.png`
- `src-tauri/icons/tray-master.png`

The project does not contain or distribute the user's local visual reference. No original reference filename, filesystem location, embedded copy, or source photograph belongs in the repository, build output, installer, or installed application.

## Generation method

The masters were produced with OpenAI image generation in reference-based redraw/edit workflows. The prompts below are concise provenance summaries, not verbatim generation transcripts and do not identify the local source file.

All character assets share the XiaoLi identity prompt: purple-gray short hair, a circular loop ahoge, a white fox-cat side mask with soft pink and apricot-gold accents, a blue-gray collar, expressive ink linework, and a restrained watercolor finish. Composition is a centered square bust with a silhouette that remains readable at small UI sizes.

| Master | Generation mode | Core prompt summary | Finalization |
| --- | --- | --- | --- |
| `mochi-green.png` | `stylized-concept`, reference-guided redraw | Calm closed-eye smile, tiny star accent, healthy and reassuring; preserve the shared identity and transparent square composition | A final image-generation transparency redraw supplied the reviewed ARGB master |
| `mochi-yellow.png` | `stylized-concept`, edit/redraw from the green identity anchor | Worried amber eyes, small sweat drop, slightly tilted side mask, restrained amber warning accent; change state expression without changing identity | Deterministic edge-background alpha cleanup |
| `mochi-red.png` | `stylized-concept`, edit/redraw from the green identity anchor | Alert focused expression, muted rose-red eyes, controlled frown, small fault diamond or exclamation; no violent or horror treatment | Deterministic edge-background alpha cleanup |
| `mochi-gray.png` | `stylized-concept`, edit/redraw from the green identity anchor | Peaceful idle expression, closed eyes, neutral mouth, softened/desaturated palette, muted blue-gray sleep accent | Deterministic edge-background alpha cleanup |
| `mochi-app-icon.png` | `logo-brand`, reference-guided redraw | Compact face, side mask, and loop-ahoge silhouette; simplified linework, cream rounded badge, safe padding, readable at 16–32 px | Deterministic edge-background alpha cleanup, then Tauri platform-icon resampling |
| `tray-master.png` | `logo-brand`, edit/redraw from the app-icon identity | Mask-and-loop-ahoge silhouette only, bold outline, very few details, clear at 16/20/24/32 px, with room for a runtime status ring | Deterministic edge-background alpha cleanup; status color is applied by the application at runtime |

The four state avatars communicate state through expression and symbol as well as color. The app and tray masters are related to the same character identity but deliberately simplified for low-resolution use.

## Mechanical alpha cleanup

`scripts/Remove-GeneratedCheckerboard.ps1` performs deterministic post-processing when image generation returns a visually neutral backdrop instead of transparent pixels. It is not an image-generation or repainting step.

The PowerShell 5.1-compatible script:

1. Loads the PNG with `System.Drawing` and creates a 32-bit ARGB destination.
2. Classifies only near-neutral dark or light pixels connected to a canvas edge.
3. Flood-fills from the outer edges, sets the connected backdrop to alpha zero, and makes retained artwork pixels opaque.
4. Saves a PNG and verifies both the ARGB pixel format and transparent corner alpha.

Because the operation is edge-connected, neutral colors enclosed inside the character or mask are not independently selected as background. The script does not change composition, expression, line placement, or state semantics. A human review at master size and at intended icon sizes remains required after processing.

## Platform icon derivation

The reviewed app-icon master is converted into Windows, macOS, and standard PNG sizes with the Tauri icon command:

```powershell
pnpm tauri icon '.\src\assets\mochi-app-icon.png' --output '.\src-tauri\icons'
```

This command performs deterministic resizing and packaging into PNG, ICO, and ICNS resources; it does not generate new character art. The tray master remains separate because its reduced silhouette and runtime status ring have different legibility requirements.

## External project boundary

No character, game asset, screenshot, logo, or illustration from the open-source projects discussed in `DESIGN.md` was used as a source asset or copied into these masters. Those projects informed interaction and information-organization principles only.

## Distribution authorization and boundary

On 2026-08-25, the user and intended publisher explicitly confirmed that they hold the rights required to use the private visual reference for this redraw and to distribute the resulting XiaoLi character assets publicly under the project's noncommercial terms. This repository records that confirmation date without publishing the private source image or its local filename.

The generated masters may therefore be included in the XiaoLi source repository and noncommercial release archives under the project's asset notice. XiaoLi v0.2.0-beta.1 reuses the reviewed masters without importing the private reference. The private reference itself remains excluded from source control, build artifacts, release archives, provenance bundles, screenshots used as source material, and installed application directories.

Required Notice: XiaoLi character and icon assets © 2026 XuYing1128. Noncommercial redistribution is permitted only together with the project license and this provenance record.
