# PLAN-002 — El MCP resuelve el proyecto por ruta, no por `use_project` manual

**Fecha:** 2026-08-19
**Fase:** 2 (Ejecución) — aprobado 2026-08-19. Rama `feature/mcp-auto-bind-por-path`
**Proyecto:** DevCtxEngine
**Origen:** Falla reproducida en un workspace real (`/home/snaven10/revfa`, 11 repos registrados):
`remember` aborta con "not bound to a project" en el 100% de las sesiones.

---

## 1. Qué resuelve

Un servidor MCP registrado globalmente arranca en el directorio desde donde se lanzó el cliente.
Cuando ese directorio es la **raíz de un workspace** —un contenedor de repos, no un repo—
`load_project()` no encuentra nada y el servidor queda sin bindear.

No degrada: **`remember` falla entero y la memoria se pierde**. El usuario no se entera salvo que
lea el error, y el protocolo que dice "guardá siempre" no se puede cumplir.

Hoy la única salida es que el agente llame `use_project` a mano en cada sesión. Eso es pedirle al
cliente que resuelva algo que el servidor ya sabe: **el registry central tiene la ruta de cada
proyecto registrado.** La información está; no se consulta.

## 2. Hallazgos verificados (2026-08-19)

Contra el código en `main` y contra un workspace real.

### 2.1 La búsqueda es solo hacia arriba

`cmd_mcp` (`crates/devctx-cli/src/main.rs:1337`) sin `--project` cae en `load_project()`, que hace
`find_config_file(&cwd)` — sube por los padres buscando `.devctx/config.yaml`. Desde la raíz de un
workspace no hay nada arriba, y **nunca mira hacia abajo**, donde están los 11 proyectos.

### 2.2 El `group` ya está poblado

Los 11 repos REVFA tienen `project.group: REVFA` en su `config.yaml`. La materia prima para resolver
por grupo **ya existe**; no hay que pedirle nada nuevo al usuario.

### 2.3 La maquinaria de rebind ya está construida

```rust
struct DevctxServer {
    backend: Arc<Mutex<Option<Arc<Backend>>>>,  // slot ya intercambiable
    connect: Connect,                            // Fn(&Path) -> Backend, para cualquier root
    cwd: PathBuf,                                // fijo al construir
}
```

`use_project` ya hace `resolve_project_root` → `connect` → swap del slot. Resolver por llamada usa
exactamente esa maquinaria. **No hay que rediseñar el estado, hay que usarlo desde otro lado.**

### 2.4 `devctx-central` no se toca

`client.list(false)` ya devuelve todas las filas con su `path`. Filtrar "proyectos bajo este cwd"
es client-side. El cambio queda contenido en `devctx-mcp` y `devctx-cli`.

### 2.5 El cwd del proceso se congela al spawn — y esto decide el diseño

El servidor **no se entera** de que el agente se movió a otro repo. Arreglar solo el arranque deja
el binding atado a dónde se lanzó el cliente, para toda la vida del proceso.

Por eso el hint por llamada **no es un extra**: es lo que hace que la resolución sea real mientras
se trabaja, y de paso elimina el `use_project` manual en cross-repo.

### 2.6 CORRECCIÓN (2026-08-19, tras revisión del usuario): no hay miembro «default»

El diseño original elegía un miembro «default» del grupo y lo usaba para TODO. Está mal, y mezcla
dos preguntas distintas:

| Pregunta | Respuesta correcta |
|---|---|
| ¿A quién se **atribuye** una memoria? | `group` → **al grupo**. `local` → al proyecto real, en su propio store. **No hay default.** |
| ¿Dónde se **busca**? | En **todos los miembros**. Un binding de grupo significa el producto; buscar en uno solo contesta otra cosa. |

Hoy `do_remember_shared` (`state.rs:1304`) saca la procedencia del store vinculado:

```rust
let project = state.project();
let (repo, branch) = state.repo_branch().unwrap_or_default();
```

En modo grupo eso es el miembro elegido por heurística — **procedencia inventada**. El campo
`project` sí se re-keyea a `@group:<nombre>` y eso estaba bien; el que miente es `repo`.

Y el fan-out es viable: `do_search_project` ya ejecuta una búsqueda con `current_dir(path)` en otro
proyecto (necesario porque **DuckDB admite un solo proceso escritor por archivo**), y el registry ya
guarda `embed_dim` documentado como *«compared before any cross-project vector work»*. La intención
estaba; la implementación no.

## 3. Tasks y orden

| Task | Qué | Depende de | Estado |
|------|-----|------------|--------|
| TASK-001 | `projects_under(path)` + resolución de grupo en el registry | — | `done` |
| TASK-002 | `enum Binding` (None / Project / Group) y `bound()`/`maybe_bound()` | — | `done` |
| TASK-003 | Descenso al arrancar, cableado en `cmd_mcp` | 001, 002 | `done` |
| TASK-004 | `unbound_help` acotado a los candidatos bajo el cwd | 001 | `done` |
| TASK-005 | Param `path` opcional en las tools de código + caché de conexiones | 002 | `done` |
| TASK-006 | Memoria en modo grupo: `scope` default `group` | 002 | `done` |
| TASK-007 | Tests de integración de los escenarios | 003, 005, 006, 009, 010, 011 | `pending` |
| TASK-008 | Docs (`mcp-integration.md` EN+ES) y ADR | 003, 005, 006 | `done` |
| TASK-009 | **Procedencia real**: `repo` = grupo en modo grupo; `local` exige proyecto | 002 | `pending` |
| TASK-010 | **Fan-out de código**: buscar en todos los miembros y fusionar por RRF | 002 | `pending` |
| TASK-011 | **Fan-out de memoria**: `recall` abarca las locales de todos los miembros | 002 | `pending` |

**Paralelizables**: 001 y 002 no se tocan entre sí. 004 sale apenas esté 001. 005, 006, 009, 010 y 011 apenas 002.

> 009, 010 y 011 salieron de la corrección §2.6 y reemplazan al «miembro default», que era la idea equivocada.

## 4. Fuera de alcance

- Cambios en `devctx-central` / el esquema del registry (§2.4: no hacen falta).
- Poblar `group` automáticamente. Sigue siendo una declaración deliberada (`project_config_for`
  lo documenta y esa decisión se respeta).
- Auto-registrar proyectos no registrados que aparezcan bajo el cwd.
- Fusionar stores de miembros con `embed_dim` distinto. Se detecta y se reporta, no se fusiona:
  vectores de dimensiones distintas no son comparables.

## 5. Cierre

<!-- SE LLENA AL CERRAR EL PLAN -->
