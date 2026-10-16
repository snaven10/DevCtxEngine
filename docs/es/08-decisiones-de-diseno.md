# Decisiones de diseño

> 🇬🇧 [Read in English](../08-design-decisions.md)

Por qué el sistema tiene la forma que tiene. Cada entrada declara la decisión,
el razonamiento y lo que cuesta — las decisiones sin costo son publicidad.

---

## ADR-01: Un solo binario Rust, sin sidecar

**Decisión.** Parseo, chunking, embeddings, reranking, almacenamiento y el
servidor MCP viven todos en un proceso. El único programa externo que se invoca
es `git`.

**Por qué.** El predecesor partía el trabajo entre dos runtimes unidos por un
puente JSON-RPC sobre stdio. Eso compraba librerías de ML nativas del lenguaje y
costaba un ciclo de vida de proceso que administrar, un límite de serialización
en cada llamada, ~880 MB residentes por cliente porque el sidecar no se podía
compartir, y una clase de falla donde una mitad estaba viva y la otra no.

**Costo.** El ecosistema de ML en Rust es más angosto. Algunos modelos existen
en Python y acá no, y a veces exportar a ONNX es el único camino.

## ADR-02: MCP sobre stdio, sin servicio de red

**Decisión.** El servidor MCP habla JSON-RPC 2.0 por stdin/stdout. Todo lo demás
que escucha — `api`, `web` — se ata a loopback.

**Por qué.** El cliente ya administra el ciclo de vida del proceso hijo, así que
stdio obtiene aislamiento y limpieza gratis. No hay puerto que colisione, ni
credencial que guardar, ni autenticación que hacer mal: el límite de confianza
es el límite del proceso.

**Costo.** Un servidor por cliente. No se soporta uso remoto.

## ADR-03: DuckDB para todo

**Decisión.** Una sola base embebida guarda vectores, índice de texto completo,
grafo de símbolos, rutas y memorias. Sin store de vectores aparte.

**Por qué.** La alternativa — un store de vectores al lado de uno relacional —
significa dos ciclos de vida, dos historias de respaldo, y ninguna forma de
filtrar vectores por un predicado relacional sin traer ambos lados a memoria.
DuckDB hace búsqueda vectorial (VSS/HNSW), BM25 (FTS) y SQL común sobre las
mismas filas, así que una búsqueda semántica filtrada es una sola consulta.

**Costo.** DuckDB permite **un solo escritor por archivo**. Esta restricción da
forma a la ADR-04.

## ADR-04: Un servidor de larga vida posee la base

**Decisión.** `devctx serve` sostiene la conexión; los comandos del CLI y las
sesiones MCP rutean a él en vez de abrir el archivo por su cuenta.

**Por qué.** Consecuencia directa de la ADR-03. Sin un dueño, una corrida de
indexado y una búsqueda pelean por el mismo lock y una de las dos falla.

**Costo.** Un proceso que supervisar. Se lanza bajo demanda y se apaga por
inactividad, y el archivo de handshake `serve.json` hay que escribirlo y
borrarlo con cuidado — un servidor que borra el archivo al salir puede dejar
inalcanzable a uno sano, que es un bug que este proyecto efectivamente publicó y
corrigió.

## ADR-05: El WAL no debe sobrevivir al proceso que lo escribió

**Decisión.** Todo camino que termina el servidor hace checkpoint primero, y
también lo hace el final de una corrida de indexado.

**Por qué.** Este es el filo más peligroso del sistema. DuckDB reproduce el WAL
al abrir, pero **un append reproducido no restaura las entradas de un índice
ART** — la estructura detrás de cada `PRIMARY KEY` y `UNIQUE` del esquema. La
tabla queda entonces con filas de las que el índice nunca se enteró, y el
siguiente `DELETE` que las toque aborta con *"Failed to delete all rows from
index"* y tumba la conexión de forma permanente. Re-indexar no lo arregla,
porque re-indexar empieza borrando.

**Costo.** Un checkpoint al final de cada corrida. `devctx repair` existe para
bases que ya están en ese estado: copia cada tabla aparte, la elimina, la recrea
desde el esquema y vuelve a escribir las filas, de modo que el índice ART se
reconstruye a partir de los datos.

## ADR-06: Tirar los índices derivados durante una carga masiva

**Decisión.** Los índices HNSW y FTS se eliminan antes de una corrida grande de
indexado y se reconstruyen después.

**Por qué.** DuckDB mantiene un índice HNSW en **cada insert**, lo que es
catastrófico durante una carga masiva — medido en 7 archivos/minuto con el
índice presente contra 58 archivos/minuto sin él, en el mismo repositorio. FTS
tiene una versión peor del problema: DuckDB no puede mantener un índice FTS
frente a borrados de filas en la tabla indexada, así que un re-indexado que
borra filas aborta de plano.

**Costo.** Una reconstrucción al final, y una ventana durante la corrida donde
la búsqueda aproximada no está disponible.

## ADR-07: Stores por proyecto; compartir solo lo que no tiene dueño

**Decisión.** Cada repositorio tiene su propia base. Un store central guarda
solo el registro de proyectos y las memorias explícitamente globales o de grupo.

**Por qué.** Un diseño anterior apuntaba todos los repositorios a una sola base.
Los stores por proyecto hacen que re-indexar un repositorio nunca bloquee a
otro, que cada uno pueda usar un modelo de embeddings distinto, y que ninguna
búsqueda necesite un filtro de repositorio para ser correcta.

**Costo.** La búsqueda entre proyectos es una llamada explícita
(`search_project`), no un default.

## ADR-08: Copias por rama, guiadas por hash de contenido

**Decisión.** Los fragmentos se guardan por `(repo, rama)`. Indexar una segunda
rama copia las filas de los archivos cuyo hash de contenido coincide, en vez de
re-embeberlas.

**Por qué.** *Esto revierte un diseño anterior.* El predecesor usaba un overlay
de ramas: un índice base más un diff. Los overlays son elegantes y acá están
mal — cada lectura paga una fusión, y la fusión tiene que saber qué lado gana
para un archivo tocado en ambos. Las copias hacen que una lectura sea una
consulta filtrada común.

Copiar es asequible porque lo caro es el embedding, no el almacenamiento, y las
ramas comparten casi todo su contenido. Medido en tres repositorios reales:
**95–96% de los archivos copiados en vez de re-embebidos**.

**Costo.** El almacenamiento crece de forma aproximadamente lineal con las ramas
declaradas. Y un caveat conocido: cambiar `indexing.exclude` entre corridas no
se refleja en el hash de contenido, así que la deduplicación puede copiar filas
que las nuevas exclusiones habrían descartado.

## ADR-09: Chunking consciente del AST, nunca partir un símbolo

**Decisión.** Los límites de fragmento salen de los parseos de tree-sitter, en
niveles archivo / clase / doc / función / bloque.

**Por qué.** El chunking de ventana fija parte una función entre dos fragmentos,
y ninguna de las mitades se embebe como aquello que la función hace. Los límites
de símbolo son las unidades sobre las que la gente hace preguntas.

**Costo.** Una gramática por lenguaje. Los archivos en lenguajes no soportados
caen a fragmentos de texto crudo con solapamiento.

## ADR-10: Indexado incremental desde el diff de git, contra el árbol de trabajo

**Decisión.** El indexado calcula qué cambió vía git, pero indexa el **árbol de
trabajo**, no el último commit.

**Por qué.** Un archivo que escribiste pero no commiteaste es exactamente el
código sobre el que más probablemente vas a preguntar.

**Costo.** Todo lo que git ignora nunca llega al índice — `.gitignore` es el
primer lugar donde controlar qué se indexa, cosa que sorprende una vez.

## ADR-11: Identidad de memoria por clave de tema, con caída a hash de contenido

**Decisión.** `--topic` hace upsert. Sin él, la identidad es un hash sobre el
contenido normalizado.

**Por qué.** Dos modos de falla distintos necesitan dos respuestas distintas.
Una decisión que se revisa tiene que reemplazarse a sí misma o el store acumula
versiones contradictorias — eso es la clave de tema. Una observación guardada
dos veces por un agente entusiasta no debe volverse dos filas — eso es el hash
de contenido.

**Costo.** Quien escribe la memoria tiene que decidir en cuál de los dos casos
está.

## ADR-12: Las memorias globales y de grupo se re-llavean, no se etiquetan

**Decisión.** Las filas globales llevan el proyecto reservado `@global`; las de
grupo llevan `@group:<nombre>`. El repositorio que las aportó sobrevive en
`repo`.

**Por qué.** La identidad deriva de proyecto + hash de contenido. Si una fila
global conservara su proyecto contribuyente, la misma lección aprendida en dos
repositorios caería como dos filas — la deduplicación fallando justo donde
compartir más importa.

**Costo.** `project` deja de ser una clave foránea común, y el código que la lee
tiene que conocer los valores reservados.

## ADR-13: La fila de unión vive en el store del proyecto

**Decisión.** Un vínculo memoria↔símbolo se escribe en la base del **proyecto** y
lleva solo el id de la memoria, incluso cuando la memoria misma vive en el
central.

**Por qué.** El grafo de llamadas es por repositorio; una memoria global no lo
es. Una memoria sobre el `charge()` de este repositorio tiene que ser
encontrable desde `charge()` sin importar cuál store guarde su texto. Resolver
el id busca primero local, después central.

**Costo.** Una indirección de búsqueda. La alternativa — copiar el texto de la
memoria a cada proyecto que la menciona — dejaría copias rancias detrás de cada
edición.

## ADR-14: La procedencia del vínculo se devuelve, no se esconde

**Decisión.** Todo resultado de memoria-por-código lleva `link_sources`:
`files-field`, `content-mention` o `inference`.

**Por qué.** Los dos primeros significan que algo conectó la memoria con el
código al momento de escribirla. El tercero significa solo que coincidieron
palabras. Colapsarlos en un solo indicador de "relacionado" presentaría una
conjetura con la misma confianza que un hecho.

**Costo.** Quien llame tiene que leer un campo para saber cuánto confiar en un
resultado.

## ADR-15: Reranking apagado por defecto

**Decisión.** El cross-encoder está deshabilitado salvo que se configure
encendido.

**Por qué.** Medición, no principio. En este repositorio una búsqueda cuesta 30
ms y 406 MB residentes; el cross-encoder más barato la lleva a 8.6 s y 2.4 GB, y
`bge-reranker-base` a 30 s y 3.4 GB. Lo que eso compra es reordenar una lista
que el recuperador ya tenía bien — y el único modelo medido contra todo el banco
la empeoró, bajando una respuesta correcta del primer puesto al vigésimo
primero.

**Costo.** El orden es el del recuperador. Todo lo que la etapa de recuperación
encontró se devuelve igual.

## ADR-16: Degradación elegante antes que falla dura

**Decisión.** La búsqueda híbrida cae a solo vectorial cuando falta el índice
FTS. El vinculado es de mejor esfuerzo y devuelve un conteo en vez de un error.

**Por qué.** Son enriquecimientos. Un repositorio todavía no indexado del todo
no debe convertir un `remember` exitoso en una falla, y un índice de palabras
clave ausente debería acotar resultados en vez de romper la consulta.

**Costo.** La degradación silenciosa es un peligro real — acá es aceptable solo
porque el resultado degradado sigue siendo correcto, apenas menos bueno. Donde
truncar sería *engañoso* en vez de meramente peor, el sistema lo dice: ver
`build_context`, que nombra lo que no entró.

## ADR-17: `build_context` devuelve prosa

**Decisión.** Una herramienta devuelve texto en vez de JSON.

**Por qué.** Su salida está pensada para leerse directo al contexto de un
modelo. Un sobre JSON alrededor de código y prosa gasta presupuesto en
puntuación y compra estructura que nadie parsea.

**Costo.** Inconsistente con la mayoría de las otras herramientas, que devuelven
JSON. `repo_map` y `skeleton` (0.10) devuelven texto por la misma razón, y
también `summarize`.

## ADR-18: Una raíz de workspace se vincula a su grupo, no a nada

**Decisión.** Cuando el directorio de trabajo del servidor MCP no contiene un
proyecto pero *contiene* proyectos registrados, desciende al registro: un solo
proyecto debajo vincula ese proyecto, varios que comparten un `project.group`
vinculan el grupo. Vinculado a un grupo, `remember` usa por defecto
`scope: group`. Las herramientas de código aceptan un `project` opcional — un
nombre o cualquier ruta dentro de uno — que resuelve una sola llamada sin mover
la sesión.

**Por qué.** Un servidor registrado globalmente hereda el directorio de trabajo
del cliente, y para quien tiene sus repositorios uno al lado del otro bajo una
carpeta, ese directorio es la raíz del workspace. Mirar solo *hacia arriba*
desde ahí no encuentra nada, así que el servidor arrancaba sin vincular — y sin
vincular no es un modo degradado: `remember` falla del todo y la memoria se
pierde. El registro sabía todo el tiempo dónde estaba cada uno de esos
proyectos.

La pista `project` existe porque el directorio de trabajo del proceso queda
fijo al lanzarlo. Una vinculación resuelta al arrancar no puede seguir a un
agente que se mueve entre repositorios, así que el trabajo entre repositorios
necesita una señal que viaje con la llamada.

**Costo.** Dos cambios de comportamiento observable. Desde una raíz de
workspace el servidor ahora arranca vinculado donde antes arrancaba sin
vincular; y en modo grupo un `remember` sin `scope` explícito cae en `group` en
vez de `local`. Lo segundo es más suave de lo que parece — las llamadas que
cambia antes fallaban en vez de escribir.

El campo `repo` de una memoria sigue saliendo de la vinculación, así que en modo
grupo nombra al miembro por defecto. La atribución es un modelo aparte del
alcance y cambiarla quedó afuera.

**Descartado.** Vincular un miembro y llamarlo el workspace: atribuiría cada
memoria a un repositorio que nadie eligió. Repartir la búsqueda de código entre
los stores de un grupo: útil, pero es otra funcionalidad con su propio problema
de ranking.

## ADR-19: Un grafo estático de tree-sitter y reglas por lenguaje — no stack-graphs, LSP ni SCIP, todavía

**Decisión.** Los símbolos y sus relaciones salen de consultas de tree-sitter
guardadas en `languages/*.json`; resolverlas entre archivos es un link pass de
reglas ordenadas, por lenguaje, escritas en Rust (imports, scopes léxicos, tipos
declarados, manifiestos). Cada arista registra qué tan segura es (`confidence`)
y por qué (`resolution`), y lo que una regla no puede decidir queda sin decidir
en vez de adivinarse.

**Por qué.** Encaja con ADR-01: un binario, sin compilador, servidor de
lenguaje ni runtime de JVM/Node en la máquina del usuario, y funciona sobre
cualquier checkout — un repositorio que no compila, una rama a mitad de un
refactor — archivo por archivo, en la misma pasada que lo chunkea, de forma
incremental. Lo que un agente de código necesita del grafo es ubicar (dónde
está, qué lo rodea, a qué llega), y los sistemas publicados que lo hacen bien
(LocAgent, RepoGraph, el repo map de Aider) usan grafos justamente de este tipo,
gruesos pero limpios. Medido sobre sitios de llamada etiquetados a mano: Java 20
de 25 correctos (19 de 19 `high`), TypeScript 20 de 20, Python 26 de 27, Rust 26
de 26.

Las alternativas, y por qué no ahora:

| Alternativa | Por qué no |
|---|---|
| stack-graphs | Archivado upstream, con un aviso abierto (RUSTSEC-2026-0303); su lenguaje de reglas igual habría que escribirlo por lenguaje |
| Un servidor LSP a demanda | Un servidor y una toolchain por lenguaje, un proyecto que compile, segundos de arranque en frío, un segundo ciclo de vida de proceso (otra vez el costo de ADR-01) y latencia por consulta |
| SCIP | Precisión de compilador, pero es un índice que produce el indexador de cada lenguaje dentro de un build (`scip-java` compila el proyecto). Queda como vía de importación futura: la columna `edges.edge_source` está reservada para eso |
| Kythe, universal-ctags | Kythe necesita una integración de build por lenguaje; ctags da definiciones, no referencias resueltas |

**Costo.** No es un compilador. No hay inferencia de tipos más allá de
declaraciones e inicializadores, las sobrecargas se eligen por aridad, no hay
análisis de flujo; las llamadas por callbacks o reflexión no dejan arista; Go
está cubierto solo con fixtures y Kotlin para nada. Las reglas son código que
mantener por lenguaje, y la salida del extractor está fijada por un test golden,
así que cada cambio suyo es un `EXTRACTOR_VERSION` y un `devctx index --full`
para cada usuario.

## ADR-20: El grafo de símbolos es aditivo; la tabla de llamadas vieja se queda una versión

**Decisión.** 0.10 agrega `symbols` y `edges` (sin clave primaria ni `UNIQUE`:
el escritor reemplaza las filas de un archivo) y sigue escribiendo
`graph_edges` de 0.9 con la extracción de 0.9.0 copiada tal cual, así que la
tabla es idéntica a la de 0.9.0. Una rama todavía no reconstruida la
responden los lectores de 0.9 con un aviso; las herramientas que solo existen
sobre el grafo nuevo se niegan con un error que nombra el arreglo. Un
reindexado se pide, nunca se hace a espaldas del usuario.

**Por qué.** Una migración en sitio sería la primera del índice, y el único
lugar donde un corte a mitad de camino corrompe lo que ya estaba. Sin claves no
hay índice ART que perder al reproducir el WAL (ADR-05). Mantener `graph_edges`
hace que volver a 0.9.0 deje un grafo que funciona, y que una rama sin
reconstruir conserve sus respuestas viejas en vez de perderlas. Un reindexado
completo es honesto sobre lo que cambió, y reusar los vectores de los chunks sin
cambios hace que cueste el parseo y no el embedding.

**Costo.** `devctx index --full` una vez por repositorio y por rama. La base
crece — las tablas nuevas, y bloques libres que DuckDB no recupera hasta la
compactación del ADR-22 — y dos caminos de lectura conviven hasta que
`graph_edges` se borre en una versión posterior.

## ADR-21: Las señales de grafo en el ranking son opt-in hasta medir que ayudan

**Decisión.** En 0.10, la centralidad de símbolos en la búsqueda híbrida
(`search.centrality_weight: 0`) y las firmas relacionadas de `build_context`
(`related: false`) vienen apagadas por defecto. El dispatch por interfaces en
`impact_analysis`, que cierra un falso negativo, viene encendido.

**Por qué.** Medición, como en ADR-15. La centralidad movió un caso de
evaluación de doce — dentro del ruido del arnés — y el único caso en español que
se movió empeoró. Las firmas relacionadas subieron el acierto del brief 3,8
puntos contra los 10 que el plan exigía para un default, por una ruta en una
línea `called by`. Apagadas, no cuestan ni cambian nada: ningún bonus, ninguna
lectura extra del grafo, y el brief armado como lo armaba 0.9.

**Costo.** Quien nunca las enciende nunca se beneficia de ellas. Encendidas,
cambian los scores híbridos y el contenido del brief, y por eso se documentan
como cambios, no como ajuste fino.

## ADR-22: El espacio se devuelve con una copia verificada y un `rename` atómico

**Decisión.** Después del `devctx index --full` que migra un índice hecho antes
del grafo, y con `devctx compact`, la base se copia fila por fila a
`<índice>.compact.tmp` con el schema de este binario, se reconstruyen ahí sus
índices HNSW y BM25, la copia se verifica contra la original (cantidad de
filas, un checksum de cada columna de cada fila, y `EXCEPT ALL` en los dos
sentidos para las memorias, sus referencias y las sesiones), y reemplaza a la
original con un `rename`. Lo hace el cliente con el servidor del proyecto
parado, nunca el servidor.

**Por qué.** Un `--full` que reusa vectores borra y vuelve a escribir cada fila
de `vectors`, y un índice actualizado termina en alrededor del doble de su
contenido. `VACUUM` no devuelve nada, y un checkpoint solo trunca los bloques
libres del final del fichero. `COPY FROM DATABASE` de DuckDB se midió y se
descartó: recrea el índice HNSW con la métrica `l2sq`, que las consultas dejan
de usar. Copiar deja la original intacta hasta el `rename`, así que cualquier
fallo la deja como estaba.

**Exclusión.** Un lock del sistema operativo sobre un fichero propio,
`<índice>.compact.lock`: cada `Store::open` lo tiene compartido mientras el
store vive, una compactación lo tiene en exclusivo. No el lock de DuckDB sobre
la base, que es un lock `fcntl` y desaparece en cuanto el proceso cierra
*cualquier* descriptor del fichero. Ese lock igual cumple su parte: una conexión
en memoria tiene la original adjunta en solo lectura desde antes de la copia
hasta después del `rename`, que es lo que deja afuera a un binario que no
conoce el fichero de lock (0.9.0), y el proceso que compacta nunca abre otro
descriptor de la original. Su write-ahead log tiene que estar ausente, y su
tamaño, mtime e inodo sin cambios, justo después de adjuntarla y justo antes
del `rename` — DuckDB aplica un log que encuentra junto a un fichero sin
comprobar que sea suyo.

**Costo.** Tanto disco libre como el fichero mientras corre, el servidor del
proyecto parado esos segundos (anunciado; solo con terminal, y apagable con
`DEVCTX_NO_AUTO_COMPACT`), y el índice HNSW reconstruido una segunda vez en el
paso. Un HNSW reconstruido no es el mismo grafo, así que su top-k aproximado
puede cambiar en algunos resultados; los datos y el top-k exacto no. El grupo,
las ACL y los atributos extendidos del fichero no se copian.
