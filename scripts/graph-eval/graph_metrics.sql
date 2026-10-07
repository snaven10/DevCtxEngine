-- Métricas del grafo (PLAN-009 TASK-001, DD-1).
--
-- Corre sobre una COPIA del index.duckdb del proyecto (run.sh la hace; el serve vivo
-- tiene abierto el archivo original). Cada consulta devuelve dos columnas (k, v):
-- el runner de run.sh las imprime como `métrica | k | v`.
--
-- Convención del archivo:
--   `-- @section legacy|new`   la sección 'new' solo corre si existen las tablas
--                              `symbols` y `edges` (TASK-003 en adelante); 'legacy' corre siempre.
--   `-- @metric <nombre>`      abre una consulta; termina en `;` al final de línea.
--
-- Las filas de la sección legacy reproducen el inventario 0.8.2 del master (§1.1) para
-- poder comparar fila a fila. "Interna" es una COTA POR NOMBRE (el nombre pelado coincide
-- con un método/función definido en el repo), igual que en el inventario: el grafo viejo
-- no tiene identidad de símbolo, así que no hay otra forma de decirlo.

-- @section legacy

-- @metric edges_total
SELECT 'filas' AS k, count(*)::VARCHAR AS v FROM graph_edges
UNION ALL SELECT 'sources_distintos', count(DISTINCT source)::VARCHAR FROM graph_edges
UNION ALL SELECT 'targets_distintos', count(DISTINCT target)::VARCHAR FROM graph_edges
UNION ALL SELECT 'target_file_poblado', count(*) FILTER (WHERE coalesce(target_file, '') <> '')::VARCHAR FROM graph_edges
UNION ALL SELECT 'metadata_poblado', count(*) FILTER (WHERE coalesce(metadata, '') <> '')::VARCHAR FROM graph_edges;

-- @metric edges_por_kind
SELECT kind AS k, count(*)::VARCHAR AS v FROM graph_edges GROUP BY kind ORDER BY count(*) DESC;

-- @metric targets_sin_calificar
-- Sin `.` ni `::` en el destino. % sobre todas las aristas.
SELECT 'n' AS k, count(*) FILTER (WHERE target NOT LIKE '%.%' AND target NOT LIKE '%::%')::VARCHAR AS v FROM graph_edges
UNION ALL
SELECT 'pct', round(100.0 * count(*) FILTER (WHERE target NOT LIKE '%.%' AND target NOT LIKE '%::%') / nullif(count(*), 0), 1)::VARCHAR FROM graph_edges;

-- @metric aristas_internas_por_nombre
-- Cota por nombre: el último segmento del destino coincide con un método/función/clase
-- definido en el repo (`vectors.symbol` de nivel método/función/clase, sin memorias).
WITH defined AS (
    SELECT DISTINCT symbol AS name FROM vectors
    WHERE coalesce(symbol, '') <> '' AND coalesce(memory_type, '') = ''
      AND symbol_type IN ('method', 'function', 'constructor', 'class', 'struct', 'impl', 'trait', 'interface', 'enum')
), e AS (
    SELECT regexp_replace(target, '^.*(\.|::)', '') AS leaf FROM graph_edges
)
SELECT 'n' AS k, count(*) FILTER (WHERE leaf IN (SELECT name FROM defined))::VARCHAR AS v FROM e
UNION ALL SELECT 'pct', round(100.0 * count(*) FILTER (WHERE leaf IN (SELECT name FROM defined)) / nullif(count(*), 0), 1)::VARCHAR FROM e;

-- @metric aristas_desde_tests
-- Aproximación en SQL de `path_kind == Test` (crates/devctx-core/src/kind.rs): directorio de
-- test o nombre de test. Es la regla del código, copiada, no el campo `kind` (que el grafo
-- viejo no guarda).
WITH t AS (
    SELECT
      regexp_matches(lower(replace(source_file, '\', '/')),
        '(^|/)(test|tests|__tests__|__mocks__|spec|specs|e2e|testdata|fixtures)/')
      OR regexp_matches(lower(regexp_replace(regexp_replace(replace(source_file, '\', '/'), '^.*/', ''), '\.[^.]*$', '')),
        '(_tests?$|^test_|\.test$|\.spec$|\.cy$)')
      OR regexp_matches(replace(source_file, '\', '/'), '(^|/)[^/]*(Test|Tests|IT)\.(java|kt|kts|scala|groovy|cs|php|swift)$')
      OR lower(regexp_replace(source_file, '^.*/', '')) = 'conftest.py' AS is_test
    FROM graph_edges
)
SELECT 'n' AS k, count(*) FILTER (WHERE is_test)::VARCHAR AS v FROM t
UNION ALL SELECT 'pct', round(100.0 * count(*) FILTER (WHERE is_test) / nullif(count(*), 0), 1)::VARCHAR FROM t;

-- @metric colisiones_nombre_pelado_con_interno
-- Destinos pelados (sin calificar) cuyo nombre coincide con un método definido en el repo
-- (`item`, `map`, `build`, `put`…): el nodo del grafo no distingue el de la librería.
WITH defined AS (
    SELECT DISTINCT symbol AS name FROM vectors
    WHERE coalesce(symbol, '') <> '' AND coalesce(memory_type, '') = ''
      AND symbol_type IN ('method', 'function', 'constructor')
)
SELECT 'n' AS k, count(*) FILTER (WHERE target NOT LIKE '%.%' AND target NOT LIKE '%::%' AND target IN (SELECT name FROM defined))::VARCHAR AS v FROM graph_edges
UNION ALL
SELECT 'pct', round(100.0 * count(*) FILTER (WHERE target NOT LIKE '%.%' AND target NOT LIKE '%::%' AND target IN (SELECT name FROM defined)) / nullif(count(*), 0), 1)::VARCHAR FROM graph_edges;

-- @metric nombres_con_varias_formas
-- Nombres (último segmento) con más de una forma calificada distinta. Dos lecturas, porque
-- el inventario 0.8.2 ("721 de 7 807; `setUp` 93, `listar` 24, `crear` 23") habla de
-- DEFINICIONES (el lado `source`: `Clase.setUp` en 93 clases), y los destinos son otra cosa.
WITH sq AS (
    SELECT DISTINCT source AS src_full, regexp_replace(source, '^.*(\.|::)', '') AS leaf FROM graph_edges
), sagg AS (
    SELECT leaf, count(DISTINCT src_full) AS formas FROM sq GROUP BY leaf
), tq AS (
    SELECT regexp_replace(target, '^.*(\.|::)', '') AS leaf, target
    FROM graph_edges WHERE target LIKE '%.%' OR target LIKE '%::%'
), tagg AS (
    SELECT leaf, count(DISTINCT target) AS formas FROM tq GROUP BY leaf
)
SELECT 'source:nombres_con_>1_forma' AS k, count(*) FILTER (WHERE formas > 1)::VARCHAR AS v FROM sagg
UNION ALL SELECT 'source:nombres_distintos', count(*)::VARCHAR FROM sagg
UNION ALL SELECT 'source:top:' || leaf, formas::VARCHAR FROM (SELECT * FROM sagg ORDER BY formas DESC, leaf LIMIT 5)
UNION ALL SELECT 'target:nombres_con_>1_forma_calificada', count(*) FILTER (WHERE formas > 1)::VARCHAR FROM tagg
UNION ALL SELECT 'target:nombres_distintos_calificados', count(*)::VARCHAR FROM tagg
UNION ALL SELECT 'target:top:' || leaf, formas::VARCHAR FROM (SELECT * FROM tagg ORDER BY formas DESC, leaf LIMIT 5);

-- @metric nodos_basura
-- `var.*` (la palabra clave `var` de Java tomada por tipo), `LOG`/`LOGGER` (una constante tomada
-- por tipo) y destinos que son expresiones (con `(`, espacio o salto de línea). Esas tres son las
-- "basura" de §6 del master. `otros_calificadores_MAYUSCULA` es informativo: mezcla constantes y
-- tipos legítimos (`UUID`, `URI`) y no entra en el total.
SELECT 'var.*' AS k, count(*) FILTER (WHERE target LIKE 'var.%')::VARCHAR AS v FROM graph_edges
UNION ALL SELECT 'LOG/LOGGER', count(*) FILTER (WHERE regexp_matches(target, '^(LOG|LOGGER)(\.|::)'))::VARCHAR FROM graph_edges
UNION ALL SELECT 'target_con_parentesis_espacio_o_salto', count(*) FILTER (WHERE regexp_matches(target, '[() \t\r\n]'))::VARCHAR FROM graph_edges
UNION ALL SELECT 'total_basura', count(*) FILTER (WHERE target LIKE 'var.%' OR regexp_matches(target, '^(LOG|LOGGER)(\.|::)') OR regexp_matches(target, '[() \t\r\n]'))::VARCHAR FROM graph_edges
UNION ALL SELECT 'otros_calificadores_MAYUSCULA', count(*) FILTER (WHERE regexp_matches(target, '^[A-Z][A-Z0-9_]+(\.|::)') AND NOT regexp_matches(target, '^(LOG|LOGGER)(\.|::)'))::VARCHAR FROM graph_edges;

-- @metric calificadores_mas_frecuentes
SELECT regexp_replace(target, '(\.|::)[^.:]*$', '') AS k, count(*)::VARCHAR AS v
FROM graph_edges WHERE target LIKE '%.%' OR target LIKE '%::%'
GROUP BY 1 ORDER BY count(*) DESC, 1 LIMIT 10;

-- @metric chunks
SELECT coalesce(nullif(chunk_level, ''), '(vacío)') || '/' || coalesce(nullif(symbol_type, ''), '-') AS k, count(*)::VARCHAR AS v
FROM vectors WHERE coalesce(memory_type, '') = ''
GROUP BY 1 ORDER BY count(*) DESC LIMIT 12;

-- @metric chunks_total
SELECT 'chunks_de_codigo' AS k, count(*) FILTER (WHERE coalesce(memory_type, '') = '')::VARCHAR AS v FROM vectors
UNION ALL SELECT 'archivos_distintos', count(DISTINCT file) FILTER (WHERE coalesce(memory_type, '') = '')::VARCHAR FROM vectors;

-- @section new
-- Sección para el esquema nuevo (`symbols` + `edges`, TASK-003 en adelante). Vacía hasta entonces:
-- el runner la salta si las tablas no existen. Mismas filas que pide §6 del master.

-- @metric edges_por_kind_nuevo
SELECT kind AS k, count(*)::VARCHAR AS v FROM edges GROUP BY kind ORDER BY count(*) DESC;

-- @metric calls_resueltas
-- % de `calls` con `dst_id`, y las no decididas (sin `dst_id` y sin marca `external`).
SELECT 'calls' AS k, count(*) FILTER (WHERE kind = 'calls')::VARCHAR AS v FROM edges
UNION ALL SELECT 'con_dst_id_pct', round(100.0 * count(*) FILTER (WHERE kind = 'calls' AND dst_id IS NOT NULL) / nullif(count(*) FILTER (WHERE kind = 'calls'), 0), 1)::VARCHAR FROM edges
UNION ALL SELECT 'no_decididas_pct', round(100.0 * count(*) FILTER (WHERE kind = 'calls' AND dst_id IS NULL AND NOT coalesce(external, false)) / nullif(count(*) FILTER (WHERE kind = 'calls'), 0), 1)::VARCHAR FROM edges
UNION ALL SELECT 'external_pct', round(100.0 * count(*) FILTER (WHERE kind = 'calls' AND coalesce(external, false)) / nullif(count(*) FILTER (WHERE kind = 'calls'), 0), 1)::VARCHAR FROM edges
UNION ALL SELECT 'from_test_pct', round(100.0 * count(*) FILTER (WHERE kind = 'calls' AND coalesce(from_test, false)) / nullif(count(*) FILTER (WHERE kind = 'calls'), 0), 1)::VARCHAR FROM edges;

-- @metric calls_por_confianza
SELECT coalesce(confidence, '(null)') || '/' || coalesce(resolution, '(null)') AS k, count(*)::VARCHAR AS v
FROM edges WHERE kind = 'calls' GROUP BY 1 ORDER BY count(*) DESC;

-- @metric basura_nuevo
SELECT 'dst_name_var' AS k, count(*) FILTER (WHERE dst_name LIKE 'var.%')::VARCHAR AS v FROM edges
UNION ALL SELECT 'dst_name_LOG', count(*) FILTER (WHERE regexp_matches(dst_name, '^(LOG|LOGGER)(\.|::)'))::VARCHAR FROM edges
UNION ALL SELECT 'dst_name_expresion', count(*) FILTER (WHERE regexp_matches(dst_name, '[() \t\r\n]'))::VARCHAR FROM edges;

-- @metric ocurrencias_repetidas
-- Filas por (src_id, dst_name) con más de una ocurrencia: la 2.ª llamada ya no se pierde.
WITH g AS (SELECT src_id, dst_name, count(*) AS n FROM edges WHERE kind = 'calls' GROUP BY 1, 2)
SELECT 'pares_con_>1_ocurrencia' AS k, count(*) FILTER (WHERE n > 1)::VARCHAR AS v FROM g;

-- @metric from_test_coherente
-- `from_test` de cada arista contra `is_test` del símbolo fuente (los dos salen de
-- `path_kind`): tiene que dar 0.
SELECT 'from_test_distinto_de_is_test' AS k, count(*)::VARCHAR AS v
FROM edges e JOIN symbols s ON s.id = e.src_id AND s.repo = e.repo AND s.branch = e.branch
WHERE coalesce(e.from_test, false) <> coalesce(s.is_test, false);

-- @metric simbolos
SELECT kind AS k, count(*)::VARCHAR AS v FROM symbols GROUP BY kind ORDER BY count(*) DESC;
