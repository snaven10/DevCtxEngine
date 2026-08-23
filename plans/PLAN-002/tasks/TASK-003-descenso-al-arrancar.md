# TASK-003 — Descenso por el registry al arrancar el servidor

- **Plan:** PLAN-002 — MCP resuelve el proyecto por ruta
- **Especialista:** —
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feature/mcp-auto-bind-por-path`
- **Depende de:** TASK-001, TASK-002
- **Estado:** `done`

---

## Objetivo

Que `devctx mcp` lanzado desde la raíz de un workspace arranque **vinculado** —al proyecto si hay uno
solo, al grupo si son varios del mismo— en vez de quedar suelto.

## Contexto verificado

`cmd_mcp` (`crates/devctx-cli/src/main.rs:1337-1374`) hoy:

```rust
let cfg = match &project {
    Some(root) => Some(ProjectConfig::load(&root.join(CONFIG_FILE_NAME))?),
    None       => load_project().ok(),      // find_config_file: solo hacia ARRIBA
};
let backend = match cfg {
    Some(cfg) => Some(mcp_backend(cfg)?),
    None      => { eprintln!("...no project here; use the use_project tool..."); None }
};
```

El comentario del código ya explica por qué NO se aborta al no encontrar proyecto (llegaría al
usuario como error de transporte). Esa decisión se mantiene: el descenso agrega un intento más
antes de rendirse, no cambia el fallback.

## Archivos

- **Modificar:** `crates/devctx-cli/src/main.rs` (`cmd_mcp`)
- **Modificar:** `crates/devctx-mcp/src/lib.rs` (`serve_stdio` / `run_stdio` para aceptar `Binding`)

## Pasos

- [ ] **Paso 1 — Orden de precedencia**, explícito y documentado:
      1. `--project <path>` (override del usuario, gana siempre)
      2. `load_project()` — cwd dentro de un repo (comportamiento actual, no se toca)
      3. **NUEVO** `resolve_under(cwd)` de TASK-001
      4. Unbound (con el mensaje mejorado de TASK-004)
- [ ] **Paso 2 — Mapear `Resolution` a `Binding`**: `Single` → `Project`, `Group` → `Group`,
      `Ambiguous`/`Empty` → `None`.
- [ ] **Paso 3 — Abrir backends con el `connect` que ya existe.** En modo grupo abrir SOLO el
      `default`; los demás miembros se abren perezosos si hacen falta (TASK-005).
- [ ] **Paso 4 — Mensaje de arranque a stderr** que diga qué resolvió y cómo:
      `Bound to group REVFA (11 projects, default REVFA_BackEnd) — resolved from /home/snaven10/revfa`.
      El actual solo dice "no project here"; el nuevo tiene que dejar rastro de la inferencia.
- [ ] **Paso 5 — `run_stdio`/`serve_stdio` aceptan `Binding`** en vez de `Option<Backend>`.

## Criterios de aceptación

- [ ] `devctx mcp` desde `/home/snaven10/revfa` arranca en modo grupo `REVFA`, y `list_projects` lo
      reporta (no `"bound": null`).
- [ ] `remember(...)` funciona SIN llamar `use_project` — el bug de origen queda cerrado.
- [ ] `devctx mcp` desde dentro de `REVFA_BackEnd` sigue bindeando a ese proyecto (sin regresión).
- [ ] `devctx mcp --project <path>` gana sobre el descenso.
- [ ] Desde un directorio sin nada registrado adentro ni arriba → arranca unbound, sin panic.
- [ ] El mensaje de stderr nombra el grupo, el default y de dónde se infirió.

## Riesgos

Si el descenso elige mal el `default`, las memorias `local` caen en el repo equivocado. Mitigado
porque en modo grupo el default de scope es `group` (TASK-006), pero el campo `repo` de la memoria
sigue saliendo del binding — declararlo en la doc (TASK-008).

## Resultado

- **Estado final:** `done`
- **Resumen:** Precedencia de 4 niveles en cmd_mcp: --project > walk upwards > descenso > unbound. Mensaje a stderr que dice que resolvio y de donde.
- **Archivos tocados:** crates/devctx-cli/src/main.rs, crates/devctx-mcp/src/lib.rs (run_stdio_bound/serve_stdio_bound)
- **Verificado por:** EN VIVO contra /home/snaven10/revfa: «Bound to group REVFA (11 projects...)». Ademas 4 tests verdes: workspace_root_binds_the_group, workspace_root_with_one_project, inside_a_repository, explicit_project_wins.
