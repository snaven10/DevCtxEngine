# TASK-014 — Set de evaluación ampliado: ≥ 40 casos en español, recall de memorias y línea base de calidad

- **Plan:** PLAN-010 — Memoria de procesos (bloque de calidad de búsqueda, decisión del usuario 2026-10-11)
- **Especialista:** general-purpose (casos a mano + scripts; poco o nada de Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`) + casos privados fuera del repo
- **Depende de:** — (después de PLAN-009; usa su arnés `scripts/graph-eval/`)
- **Estado:** `pending`

---

## Objetivo

Tener un set de evaluación con poder suficiente para decidir los cambios de calidad (TASK-015…019)
y una línea base medida **antes** de tocar nada. Hoy el español tiene 13 casos: cada caso vale ~8 pp
y el ruido entre corridas es de ±1 caso, así que una mejora real de 2 casos no se distingue del azar.
PLAN-010 DD-12.

## Contexto verificado

- Arnés de PLAN-009 TASK-001: `scripts/graph-eval/run.sh` + `score.py` (Hit@k, MRR, presencia en el
  brief), formato `consulta | archivo(s) | símbolo | repo | idioma`, `search --no-rerank --limit 20`
  vectorial e `--hybrid`, brief = `devctx context --max-tokens 4096 --no-memories`.
- Casos en el repo: `scripts/graph-eval/cases-devctx.txt` (10, de los que 3 en español). Casos
  privados: fuera del repo por `$DEVCTX_EVAL_CASES` (regla Q-5 de PLAN-009); 20 casos en 4 repos
  (backend-a, backend-b, frontend, legacy-migration).
- Línea base de PLAN-009 TASK-001 (0.9.0): por idioma, ES (13) vector Hit@5 8/13, MRR 0.408;
  híbrido Hit@5 6/13, MRR 0.435. EN (17) vector 14/17, MRR 0.662; híbrido 15/17, MRR 0.760. Ruido:
  ±1 posición entre corridas del mismo índice → TASK-016 de PLAN-009 corre 2-3 veces y usa la mediana.
- El arnés no mide `recall` de memorias, y el cambio de pooling (TASK-017) también mueve ese espacio.
- `rerank` está apagado en todas las corridas del arnés (`--no-rerank`).

## Archivos

- **Modificar:** `scripts/graph-eval/cases-devctx.txt` (casos nuevos de DevCtxEngine, ES y EN),
  `scripts/graph-eval/run.sh` y `score.py` (corte por idioma y por tipo de consulta; modo `--rerank`
  opcional con latencia; sección de recall), `scripts/graph-eval/README.md`.
- **Crear:** `scripts/graph-eval/recall-devctx.txt` (casos de recall sobre memorias sintéticas
  versionadas en el repo, sin datos de clientes) y su fixture de memorias
  (`scripts/graph-eval/memories-devctx.jsonl`).
- **Crear fuera del repo (con OK del usuario, Q-5):** casos nuevos de los repos privados en la carpeta
  privada que ya usa `$DEVCTX_EVAL_CASES`, y `$DEVCTX_EVAL_RECALL` para recall privado si se quiere.

## Pasos

- [ ] **Paso 1 — casos nuevos, escritos antes de medir.** Meta: **≥ 40 en español** y ≥ 30 en
      inglés en total (repo + privados). Reparto sugerido: DevCtxEngine +15 ES / +5 EN (en el repo);
      privados +15 ES (backend-a, frontend, legacy-migration). Tipos: ≥ 1/2 de "comportamiento"
      (no nombra el identificador), ≥ 1/4 que usan términos en español sobre código con
      identificadores en inglés (el caso que motiva TASK-015/016), algunos con identificador en
      camelCase/snake_case partido en palabras ("buscar usuario por id" → `findUserById`). Cada caso
      con un campo extra opcional `tipo` (`comportamiento|identificador|mixto`) que `score.py` agrupa.
- [ ] **Paso 2 — regla de oro.** Targets etiquetados leyendo el código **antes** de correr nada;
      el commit de los casos del repo va antes que cualquier medición; los privados se fechan en un
      `README` de su carpeta. Errores de transcripción se anotan en el Resultado, nunca se "arreglan"
      mirando el ranking.
- [ ] **Paso 3 — recall de memorias.** ~40 memorias sintéticas sobre DevCtxEngine (mitad ES, mitad
      EN, escritas para el test, sin datos reales) en un JSONL versionado; ≥ 20 consultas de recall
      con la memoria esperada. `run.sh` las importa a un store temporal (`devctx memories import`
      sobre un `HOME`/central de prueba) y mide Hit@3 y MRR. Nunca toca el central real.
- [ ] **Paso 4 — métricas y cortes.** `score.py` reporta por idioma × modo (vector, híbrido,
      keyword) × tipo: Hit@1/5/10, MRR, brief archivo/símbolo, y para recall Hit@3/MRR. Intervalo
      simple: bootstrap de 1000 remuestreos por caso → IC 90 % de la diferencia entre dos corridas
      (stdlib, `random` con semilla fija).
- [ ] **Paso 5 — ruido.** 3 corridas del mismo índice; reportar mediana y rango. Los umbrales (por
      task y por bloque) son contra la mediana.
- [ ] **Paso 6 — línea base congelada.** Correr sobre el binario de `main` al arrancar esta task (sin
      ningún cambio de PLAN-010 de calidad) y guardar `report.md` en el Resultado: es el "antes" de
      TASK-015…019.

## Criterios de aceptación

- [ ] ≥ 40 casos ES y ≥ 30 EN etiquetados, con el corte por tipo, y fecha/commit anterior a cualquier
      medición de TASK-015…019.
- [ ] `run.sh` produce las tablas por idioma × modo × tipo y la de recall, con mediana de 3 corridas
      e IC de la diferencia.
- [ ] Línea base en el Resultado (sin rerank, y con el reranker que el usuario tenga configurado hoy
      como columna aparte si está disponible).
- [ ] `git grep` de privacidad del plan vacío sobre todo lo commiteado; los casos privados no están en
      el repo.

## Umbrales que usan las tasks siguientes (PLAN-010 DD-12)

Dos reglas, una por task y otra por bloque. Todas se miden contra la mediana de 3 corridas.

**Regla por task (adopción de cada cambio de TASK-015…019).** Se compara contra el punto de partida
de la task, o sea la línea base de esta task más lo ya adoptado. Se adopta si:
- **ES:** la mediana mejora en Hit@5 o en MRR ES (mejora puntual > 0);
- **ninguna regresión con IC:** en ES, EN, brief y recall, el IC 90 % de la diferencia **no** queda
  entero por debajo de 0; además EN no baja más de 1 caso en Hit@5 ni más de 0.02 de MRR, y brief y
  recall no bajan más de 1 caso (salvo recall en TASK-017, donde se espera que suba);
- **costo:** indexado ≤ +20 % de tiempo, `search` p95 ≤ +10 % (sin rerank).

**Regla por bloque (meta del bloque, se verifica en TASK-011 contra la línea base de esta task).**
- **ES:** Hit@5 ≥ +10 pp (≈ +4 casos sobre 40) **o** MRR ES ≥ +0.05, con el IC 90 % de la diferencia
  sin cruzar 0;
- EN, brief y recall: los mismos límites de no regresión de la regla por task.

## Riesgos

- **Casos escritos por quien ya conoce los cambios.** Mitigación: Paso 2 y que el usuario revise una
  muestra de los privados antes de medir.
- **Sesgo hacia el repo propio.** DevCtxEngine es pequeño y en inglés en el código; por eso la mitad
  de los ES nuevos va a los repos privados.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-010 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
