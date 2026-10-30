# Characterize approved existing behavior

Read the approved plan and independent test brief. Add only the specified tests and fixtures within the configured test paths. Use the exact required fullName IDs, independently meaningful assertions and real boundaries required by the plan. Assert intended existing behavior; do not weaken expectations or change production code to obtain a pass.

Run lint and the configured Vitest JSON command. Every planned new assertion must execute once and pass; skipped, pending, todo, missing or duplicate required IDs remain blocks. Preserve complete command outputs and commit the tests before reporting done. Do not claim RED for passing characterization. A production defect, requirement conflict or necessary production edit is scope_change and requires a revised plan with fresh Owner GO. Report only this step's outcome; the hub owns freeze tags, routing, review and PR publication.
