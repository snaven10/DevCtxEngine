# Propuesta: overhaul de instalación y configuración (doc + defaults + receta multi-repo)

**Estado:** Borrador / planificación
**Fecha:** 2026-06-21
**Motivación:** un usuario que conoce el proyecto reporta que "es difícil entender cómo instalar y configurar". El diagnóstico (3 auditorías: doc, maquinaria, setup vivo) confirma que **no es falta de documentación** — hay docs extensas y bilingües — sino **fragmentación, contradicciones internas, y defaults que pelean contra el setup correcto**. Esta propuesta consolida la doc en una sola fuente de verdad, arregla los defaults de mayor impacto, y documenta la topología "store central multi-repo" como receta reproducible.

---

## 1. Diagnóstico (con evidencia)

Tres frentes, cada uno verificado contra el código/filesystem real.

### Frente 1 — La documentación se contradice a sí misma

| Problema | Evidencia | Impacto |
|---|---|---|
| Instalación en **4 lugares** distintos | `README.md` §Install (L27–52), §Installation (L133–160), `DOCS.md` §Quick Install (L14–29), `docs/11-configuration.md` §2.4 (L160–178) | El usuario no sabe cuál seguir |
| Contradicción "curl vs from-source" | README L27 dice script precompilado como camino principal; L134 dice "From Source (**recommended**)" | ¿Cuál es el recomendado? |
| `DOCS.md` está **podrido** | 6 links muertos (`docs/setup.md`, `docs/architecture.md`, `docs/api.md`, `docs/mcp-tools.md`, `docs/schemas.md`, `docs/features.md` — ninguno existe); dice "No releases yet" (van por v0.12.0); dice "17 MCP tools" (son 21) | Callejón sin salida |
| `config.yaml` schema desactualizado en README | README §Configuration (L367–395) omite `language:`, `runtime.python_path:`, `storage.local_db_path:` que sí están en `docs/11` §1.1 | Schema incompleto |
| Env vars contradictorias | README (7 vars) vs `docs/11` §3 (30+). `DEVAI_STATE_DIR` default sale relativo (`.devai/state/`) en README, global (`~/.local/share/devai/state`) en docs/11. `DEVAI_TOKEN_STRATEGY` en README le falta el valor `hard_truncate` | Valores incorrectos en la "referencia rápida" |
| Tabla de modelos sin ONNX | README §Embedding Models (L427–432) lista 6 modelos torch; faltan `ml-granite` y `ml-granite-lg` (ONNX, los recomendados para CPU, agregados en v0.11.0) | El usuario no ve la mejor opción |
| Env vars de v0.12 ausentes de la tabla autoritativa | `DEVAI_EMBED_MAX_CHARS` / `DEVAI_EMBED_BATCH_SIZE` (previenen OOM) solo están en `docs/09`, no en `docs/11` §3 | La guía de config "completa" no las tiene |
| Comando MCP inconsistente | README usa `devai server configure --all`; `docs/01` L95 usa `devai server configure claude` | Duda sobre la sintaxis correcta |
| Ciclo de navegación | README §Install → `docs/01` Documentation Map (L132) → `README.md#install` | Vuelta en círculo |

**Veredicto:** no existe un único camino lineal de cero a IDE-conectado. Hay que visitar mínimo 3 archivos y se recibe info contradictoria en al menos 4 puntos.

### Frente 2 — Los defaults pelean contra el setup correcto (footguns)

Verificado en código. Los relevantes para el dolor del usuario:

| # | Footgun | Archivo:línea | Default sano |
|---|---|---|---|
| F | `devai init` escribe un `state_dir` **absoluto per-repo** → rompe la centralización: indexás en `<repo>/.devai/state` pero el MCP busca en el store central | `init.go:61, 84` | No escribir `state_dir` en el config generado → usar el XDG central por default |
| C | `devai index` sin `--config` usa el `.devai/config.yaml` **del repo**; si su modelo difiere del store, lo corrompe (mismatch de dimensiones LanceDB) | `cmd/devai/cmd/server.go:79–96`, `config.go:86–98` | Resolver primero el config del state dir, o ligar el modelo al store |
| D | El hook auto-index corre `index --incremental` **sin `--config` ni `DEVAI_EMBEDDING_MODEL`** — solo hereda `DEVAI_STATE_DIR` | `hooks.go:55–59` | Inyectar el modelo activo + `DEVAI_EMBED_MAX_CHARS` en el bloque del hook al instalarlo |
| E | Al instalar el hook sin `DEVAI_STATE_DIR` en el env, cae al default per-repo (`<repo>/.devai/state`) en vez del store central | `hooks.go:84–87` | Advertir si `DEVAI_STATE_DIR` no está seteado y documentar el modo central |
| A | Wheel ML faltante en el release → `warn` y continúa: las features ML no andan **en silencio** | `install.sh:413` | `die` con mensaje claro, o verificar antes de continuar |
| G | `DEVAI_EMBED_MAX_CHARS` / `DEVAI_EMBED_BATCH_SIZE` no se inyectan en el cliente MCP ni en el hook → dependen de un default silencioso del Python | `install.sh:469–475` | Inyectar `DEVAI_EMBED_MAX_CHARS` explícito en `configure_client` |

(Footguns B y H del audit — hook en directorio equivocado del installer, y ONNX requiere AVX2 — se cubren con doc, no con cambio de default.)

**Default real de modelo confirmado:** `minilm-l6` (384d, inglés) — `ml/devai_ml/config.py:100`, `init.go:66`, `install.sh:31`. Consistente entre wizard, init y código.

### Frente 3 — El setup vivo del usuario es el "camino experto" indocumentable

La topología real en esta máquina (verificada en filesystem) **ya resuelve a mano** casi todos los footguns de arriba. Es lo que el default debería producir solo:

```
REVFA_BackEnd  (hook post-commit) ─┐
REVFA_FrontEnd (hook post-commit) ─┤
REVFA_Calidad  (hook post-commit) ─┼─→ store CENTRAL: /home/snaven10/revfa/.devai/state/
REVFA_Auth     (hook post-commit) ─┘        ├─ index.db        (SQLite, ~557 MB)
                                            └─ vectors/vectors.lance  (LanceDB, ~12 GB, modelo ml-granite / 384d)
MCP server (devai server mcp) ──────────────→ MISMO store central
```

- Los 4 `config.yaml` apuntan al mismo `state_dir` (`/home/snaven10/revfa/.devai/state`) y al mismo modelo (`ml-granite`).
- Cada hook post-commit inyecta explícito: `DEVAI_STATE_DIR`, `DEVAI_LOCAL_DB_PATH`, `DEVAI_EMBED_MAX_CHARS=2048`, y corre `index --incremental &` en background.
- `REVFA_BackEnd` y `REVFA_FrontEnd` tienen **guards de worktree** (`case "$toplevel" in *REVFA_Frontend_desp) exit 0`) para no indexar los worktrees que comparten el hook vía gitdir (evita el phantom bug).
- `~/.local/share/devai/state/vectors` es un **symlink** al store central del workspace.
- El MCP (`.mcp.json`) apunta al mismo `DEVAI_STATE_DIR` / `DEVAI_LOCAL_DB_PATH`.

**Un usuario nuevo no puede reproducir esto leyendo la doc actual.** Esta es la receta que falta documentar.

---

## 2. Objetivos y no-objetivos

**Objetivos**
1. Un único camino lineal de instalación de cero a IDE-conectado, sin contradicciones.
2. `docs/11-configuration.md` como **fuente de verdad única** de configuración; el README como resumen con punteros, no como referencia paralela divergente.
3. Eliminar los footguns de mayor impacto vía mejores defaults (centralización, hook, fallo del wheel).
4. Documentar la topología "store central multi-repo" como receta reproducible.
5. Paridad EN/ES en toda la doc tocada.

**No-objetivos**
- No reescribir las docs de conceptos (`docs/02`–`docs/10`) más allá de los fixes de consistencia puntuales.
- No cambiar el modelo default ni el formato del store.
- No tocar la lógica de indexación/embeddings salvo los defaults de configuración listados.
- No migrar el store del usuario (su setup ya está sano); la receta lo documenta, no lo modifica.

---

## 3. Diseño por workstream

### WS1 — Consolidar la documentación (fuente de verdad única)

**Principio:** `docs/11-configuration.md` es autoritativo para config. El README **resume y apunta**, nunca duplica tablas que puedan divergir.

Cambios:

1. **`README.md` §Install + §Installation → fusionar en UNA sección "Install".**
   - Camino primario: `curl | bash` (script). Camino secundario claramente etiquetado: "From source (for contributors)".
   - Eliminar la etiqueta contradictoria "(recommended)" del from-source.
   - Tabla de flags del installer: completar los 7 flags reales (`--install-dir`, `--state-dir`, `--model`, `--client`, `--scope`, `--hooks/--no-hooks`, `--yes`) **o** puntero explícito a `docs/11` §2.4 como única tabla. Decisión: puntero (evita re-divergencia).

2. **`README.md` §Configuration → reducir a "quick reference" + puntero.**
   - Dejar solo las 5–7 env vars más comunes, marcadas como "los defaults comunes; tabla completa en `docs/11-configuration.md` §3".
   - Corregir el schema `config.yaml` mostrado (agregar `language:`, `runtime.python_path:`, `storage.local_db_path:`; quitar `__pycache__/**` si no aplica) **o** reemplazarlo por un puntero a `docs/11` §1.1.
   - Corregir valores: `DEVAI_STATE_DIR` default = `~/.local/share/devai/state`; `DEVAI_TOKEN_STRATEGY` = `drop / soft_truncate / hard_truncate / summarize`.

3. **`README.md` §Embedding Models → agregar `ml-granite` y `ml-granite-lg`** (ONNX, 384/768d) y marcar `ml-granite` como recomendado para CPU multilingüe. Puntero a `docs/09` §1 para la guía "cuál elegir" y a §5 para la tabla por hardware.

4. **`docs/11-configuration.md` §3 → agregar `DEVAI_EMBED_MAX_CHARS` (4096) y `DEVAI_EMBED_BATCH_SIZE` (16/8)** a la tabla de env vars con su rol (guarda anti-OOM). Es la tabla "completa autoritativa" y hoy no las tiene.

5. **`DOCS.md` → eliminar** (o reducir a un redirect de 1 línea a `docs/`). Está podrido: 6 links muertos, "no releases" obsoleto, "17 MCP tools" incorrecto. El README §Documentation ya es el índice real.

6. **Romper el ciclo de navegación:** `docs/01` Documentation Map y el README deben tener UNA dirección de flujo (README = entrada → docs/ = profundidad). Corregir el comando MCP a la forma canónica única (`devai server configure --all` o la posicional, la que el binario realmente acepte — verificar contra `cmd/`).

7. **Paridad ES:** replicar 1–4 en `docs/es/11-configuracion.md` y `docs/es/01-introduccion.md`.

### WS2 — Mejores defaults (código Go + scripts)

Cada cambio es independiente y verificable. Orden por impacto:

1. **`init.go:61, 84` — no escribir `state_dir` per-repo.** El config generado por `devai init` omite `state_dir` (o lo deja vacío) para que resuelva al store XDG central por default. *(Fix #1 de centralización — footgun F.)*
   - Verificar: tras `devai init` en un repo limpio, `index` + el MCP usan el mismo store sin config manual.

2. **`hooks.go:55–59, 84–87` — el bloque del hook captura el contexto del store.** Al instalar, inyectar en el bloque: `DEVAI_EMBEDDING_MODEL` (modelo activo resuelto del state/env) y `DEVAI_EMBED_MAX_CHARS`. Advertir si `DEVAI_STATE_DIR` no está seteado al instalar. *(Footguns D, E.)*
   - Resultado esperado: el hook generado se parece al que el usuario armó a mano.

3. **`install.sh:413` — wheel ML faltante = error claro.** Cambiar `warn` por `die` (o flag `--allow-no-ml` explícito para el caso intencional). *(Footgun A.)*

4. **`install.sh:469–475` — `configure_client` inyecta `DEVAI_EMBED_MAX_CHARS`** (y opcionalmente `DEVAI_EMBED_BATCH_SIZE`) en el JSON del cliente MCP, visible y ajustable. *(Footgun G.)*

5. **(Opcional, evaluar en plan) `server.go` / `config.go` — resolución de config del index.** Que `devai index` sin `--config` prefiera el config del state dir sobre el del repo, ligando el modelo al store. Mayor riesgo (cambia semántica de resolución) → se decide en el plan si entra o se difiere. *(Footgun C.)*

**Riesgo:** son cambios de comportamiento en un binario alpha. Mitigación: cada cambio con su test (Go test donde exista cobertura de `init`/`hooks`; smoke manual del install.sh en repo limpio). Registrar todo en CHANGELOG como breaking-ish (alpha).

### WS3 — Documentar la receta "store central multi-repo"

Nueva sección/doc (decisión en plan: sección nueva en `docs/11` §4 ampliada, **o** doc dedicada `docs/12-multi-repo-central-store.md` + espejo ES). Contenido:

1. **El modelo mental:** un store central, N repos que lo alimentan vía hooks, un MCP que lo lee. Diagrama de la topología real (el de §1 Frente 3, generalizado).
2. **Receta paso a paso reproducible:**
   - Elegir el path del store central (ej. raíz del workspace `.devai/state` o `~/.local/share/devai/state`).
   - Por repo: `config.yaml` con `state_dir` apuntando al store central + modelo consistente.
   - Instalar hooks que inyecten `DEVAI_STATE_DIR` + `DEVAI_LOCAL_DB_PATH` + `DEVAI_EMBED_MAX_CHARS`.
   - Configurar el MCP (`.mcp.json`/`settings.json`) al mismo store.
3. **Worktree guards:** por qué y cómo (el `case "$toplevel"` que evita el phantom bug cuando un worktree comparte el hook del repo padre vía gitdir).
4. **Gotchas conocidos** (de las memorias del usuario, generalizados): consistencia de modelo entre repos, el `--config` y el mismatch de dimensiones, el cap de RAM/`DEVAI_EMBED_MAX_CHARS`, no commitear `.mcp.json` (credenciales).
5. **Nota de transición:** una vez aplicado WS2 (init central por default + hook con env vars), esta receta se vuelve "casi automática"; la doc refleja ambos mundos (manual hoy, asistido tras WS2).

---

## 4. Verificación

- **WS1/WS3 (doc):** lectura lineal de prueba — alguien sigue README §Install → Quick Start → IDE conectado sin saltar de archivo ni encontrar contradicción. Checklist de las 4 contradicciones cerradas. `rg` de links en `DOCS.md`/README para confirmar 0 links muertos. Paridad EN/ES (mismos campos en ambos).
- **WS2 (código):**
  - `devai init` en repo limpio → `cat .devai/config.yaml` no tiene `state_dir` per-repo; `index` + MCP comparten store.
  - `devai hooks install` → el `.git/hooks/post-commit` generado contiene `DEVAI_EMBEDDING_MODEL` y `DEVAI_EMBED_MAX_CHARS`.
  - `install.sh` sin wheel → falla con mensaje claro (no silencioso).
  - Tests Go existentes verdes; agregar test de `init`/`hooks` si hay harness.
- **No buildear sin pedido del usuario** (regla del proyecto). El build/smoke se corre cuando el usuario lo autorice.

---

## 5. Rollout

- Rama: `docs/install-config-overhaul` (ya creada).
- Un PR (o dos: doc primero, código después) — a decidir en el plan según tamaño del diff.
- CHANGELOG: entrada describiendo la consolidación de doc + los cambios de default (marcar los de comportamiento).
- Sin release nuevo salvo que el usuario lo pida (los cambios de install.sh/wheel solo llegan a usuarios con un release nuevo; los de doc no).

---

## 6. Decisiones abiertas (para el plan)

1. README: ¿punteros a `docs/11` o tablas resumidas mantenidas? → **Propuesta: punteros** (evita re-divergencia).
2. `DOCS.md`: ¿eliminar o redirect de 1 línea? → **Propuesta: eliminar** (el README §Documentation es el índice).
3. WS3: ¿sección en `docs/11` o doc nueva `docs/12`? → a decidir por tamaño del contenido.
4. Footgun C (resolución `--config`): ¿entra en este esfuerzo o se difiere? → mayor riesgo, decidir en plan.
5. ¿Un PR o dos (doc / código)? → decidir por tamaño del diff.
