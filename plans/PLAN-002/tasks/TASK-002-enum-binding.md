# TASK-002 — `enum Binding` de primera clase (None / Project / Group)

- **Plan:** PLAN-002 — MCP resuelve el proyecto por ruta
- **Especialista:** —
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feature/mcp-auto-bind-por-path`
- **Depende de:** — (paralela a TASK-001)
- **Estado:** `done`

---

## Objetivo

Que el servidor pueda estar vinculado a un GRUPO y no solo a un proyecto, sin que las tools de
código pierdan la garantía de tener un backend concreto.

## Contexto verificado

Hoy el estado es (`crates/devctx-mcp/src/lib.rs:321-346`):

```rust
struct DevctxServer {
    backend: Arc<Mutex<Option<Arc<Backend>>>>,
    connect: Connect,          // Fn(&Path) -> Result<Backend, String>
    cwd: PathBuf,
}
fn bound(&self)       -> Result<Arc<Backend>, ErrorData>  // error = unbound_help(cwd)
fn maybe_bound(&self) -> Option<Arc<Backend>>             // para tools de registry
```

`Option<Arc<Backend>>` no puede expresar "estoy en el grupo REVFA, con 11 miembros, ninguno elegido".
Ese es el estado nuevo.

## Archivos

- **Modificar:** `crates/devctx-mcp/src/lib.rs`

## Pasos

- [ ] **Paso 1 — Definir el enum**, reemplazando el `Option` del slot:
      ```rust
      enum Binding {
          None,
          Project(Arc<Backend>),
          Group { name: String, members: Vec<ProjectRow>, default: Arc<Backend> },
      }
      ```
      `default` es el miembro con el que se resuelven las tools de código cuando no hay hint —
      elegir el **más recientemente indexado** (`last_indexed_at`), que es el que probablemente se
      está trabajando. Documentar por qué.
- [ ] **Paso 2 — `bound()`**: `Project` → ese backend. `Group` → el `default`, pero registrando que
      la elección fue implícita (lo usa TASK-005 para decidir si avisar).
- [ ] **Paso 3 — `maybe_bound()`**: devuelve backend en `Project` y en `Group`; `None` solo en `None`.
- [ ] **Paso 4 — `is_group()` / `group_scope_default()`**, que TASK-006 consume.
- [ ] **Paso 5 — `use_project` sigue funcionando** y pasa el binding a `Project` (override explícito
      del modo grupo). No romper su contrato de salida (`{bound, path}`).
- [ ] **Paso 6 — Extender la salida de `list_projects`** con el binding actual: `{"bound": null}` hoy
      no distingue "sin bindear" de "en grupo". Devolver `{"bound": {"kind": "group"|"project", ...}}`.

## Criterios de aceptación

- [ ] Compila sin warnings y ningún call site de `bound()`/`maybe_bound()` queda roto.
- [ ] En modo `Group`, una tool de código funciona (usa el `default`) en vez de fallar.
- [ ] `use_project` desde modo grupo pasa a `Project` y una llamada posterior lo confirma.
- [ ] `list_projects` distingue los tres estados de binding.
- [ ] Los tests existentes de MCP siguen verdes sin editarlos.

## Riesgos

Cambiar el tipo del slot toca todos los call sites. Riesgo de compilación, no de runtime: el
compilador los señala todos. Hacerlo en un commit propio para que el diff sea legible.

## Resultado

- **Estado final:** `done`
- **Resumen:** `enum Binding {None, Project, Group{name,members,default,default_name}}` reemplaza al `Option<Arc<Backend>>`. `bound()`/`maybe_bound()` conservan su firma, asi que los 23 call sites quedaron intactos.
- **Archivos tocados:** crates/devctx-mcp/src/lib.rs
- **Verificado por:** Compila sin warnings; `list_projects` devuelve `binding` con kind project/group/null.
- **Desviaciones:** El `default` ya NO se usa para atribuir memoria (ver TASK-009) ni para buscar (TASK-010): quedo solo como fallback de las tools de codigo cuando no hay hint. `is_group()` se escribio y despues se removio al quedar muerto.
- **Riesgos abiertos / siguiente:** El criterio del default (mas recien indexado) es INESTABLE: un reindex de fondo lo cambia. Con 009 y 010 ya casi no importa, pero sigue decidiendo el fallback sin hint.
