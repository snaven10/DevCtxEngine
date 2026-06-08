# Propuesta: soporte de backend ONNX en embeddings locales

**Estado:** Borrador / planificación
**Motivación:** habilitar modelos de embedding en formato ONNX (cuantizado int8) para indexar más rápido en CPU, con mejor calidad y menos almacenamiento que el modelo multilingüe actual (`ml-mpnet`).

---

## 1. Motivación (con datos)

Benchmark sobre corpus de dominio real (49 documentos / 40 búsquedas), CPU-only:

| Modelo | Motor | dim | Recall@1 | MRR | textos/s | Peso disco | RAM pico |
|---|---|---|---|---|---|---|---|
| `ml-mpnet` (actual) | PyTorch | 768 | 87.5% | 0.921 | 17.5 | 1060 MB | 1248 MB |
| e5-base cuant | ONNX int8 | 768 | 82.5% | 0.906 | 22.3 | 270 MB | 559 MB |
| granite-97m | PyTorch | 384 | 95.0% | 0.975 | 9.3 | 186 MB | 841 MB |
| **granite-97m** | **ONNX int8** | **384** | **95.0%** | **0.975** | **58.7** | **94 MB** | **822 MB** |
| granite-311m | ONNX int8 | 768 | 92.5% | 0.963 | 15.0 | 299 MB | 1177 MB |

**Conclusión:** `ibm-granite/granite-embedding-97m-multilingual-r2` con el peso `onnx/model_quint8_avx2.onnx` gana en las cuatro dimensiones frente al modelo actual: mejor recall (95% vs 87.5%), **6.3x más rápido de indexar** (58.7 vs 9.3 txt/s del mismo modelo en torch), **mitad de dimensión** (384 → mitad del LanceDB), y el peso en disco más liviano (94 MB). No requiere prefijos `query:`/`passage:`.

**El bloqueo actual:** `LocalEmbedding.__init__` carga con `SentenceTransformer(name, device=device)` — sin `backend` — así que el modelo correría en torch (9.3 txt/s), desperdiciando el ONNX. El soporte de backend ONNX es lo que desbloquea estos números.

---

## 2. Diseño

Hacer el backend ONNX **genérico** (cualquier modelo del registry puede declararse ONNX), no un hack para un solo modelo. La clave: extender `ModelInfo` con metadatos de backend y consumirlos al construir el `SentenceTransformer`.

### Decisión de diseño
- Aprovechar el soporte nativo de `sentence-transformers` (`backend="onnx"` + `model_kwargs={"file_name": ...}`), introducido en ST 3.2. No reinventar la carga ONNX.
- El registry de Python (`MODEL_REGISTRY` en `embeddings/local.py`) sigue siendo la **fuente de verdad única**; el CLI Go valida vía RPC `model_list`, así que agregar la key ahí la expone automáticamente.

---

## 3. Cambios por archivo

### 3.1 `ml/pyproject.toml` — dependencias
```toml
sentence-transformers >= 3.2.0   # backend="onnx" requiere >= 3.2 (instalado: 5.5.1)
optimum[onnxruntime] >= 1.23.0   # exportador/cargador ONNX
# onnxruntime ya entra transitivo (flashrank), declararlo explícito no está de más
```

### 3.2 `ml/devai_ml/embeddings/local.py` — registry + carga

**a) Extender `ModelInfo`** (agregar a `__slots__` con defaults retro-compatibles):
```python
backend: str = "torch"        # "torch" | "onnx"
onnx_file: str | None = None  # p.ej. "onnx/model_quint8_avx2.onnx"
```

**b) Nueva entrada en `MODEL_REGISTRY`:**
```python
"granite-97m": ModelInfo(
    name="ibm-granite/granite-embedding-97m-multilingual-r2",
    dimension=384, size_mb=94, speed="fast", quality="best",
    backend="onnx", onnx_file="onnx/model_quint8_avx2.onnx",
    desc_es="Granite 97M multilingüe en ONNX int8. La mejor relación calidad/velocidad en CPU: 95% recall, indexa 6x más rápido, mitad de almacenamiento. No requiere prefijo.",
    desc_en="...",
),
```

**c) `LocalEmbedding.__init__`** — leer el `ModelInfo` completo (hoy usa `MODELS[key] = (name, dim)`; pasar a `MODEL_REGISTRY[key]`) y construir condicionalmente:
```python
info = MODEL_REGISTRY[model_key]
st_kwargs = {"device": device}
if info.backend == "onnx":
    st_kwargs["backend"] = "onnx"
    st_kwargs["model_kwargs"] = {"file_name": info.onnx_file}
self._model = SentenceTransformer(info.name, **st_kwargs)
```

**d) `_model_is_cached`** — para modelos ONNX, verificar que el snapshot contenga el `onnx_file`, no solo metadata, para no marcar "cacheado" un modelo sin su peso ONNX.

### 3.3 Descarga (`model_download` / `update`)
Asegurar que la descarga incluya el subdirectorio `onnx/`. `SentenceTransformer(backend="onnx", model_kwargs={"file_name": ...})` lo baja on-demand; si `model download` usa `snapshot_download` con `allow_patterns`, agregar `"onnx/*"`.

### 3.4 Lado Go (`cmd/devai/cmd/model.go`)
- **Sin cambios funcionales**: la validación de `model use` consulta el registry Python por RPC, así que `granite-97m` se acepta solo.
- **Cosmético**: actualizar el help text de `model use` (línea ~46) para listar la nueva key.
- Confirmar que el RPC `model_list` / `ModelInfo.to_dict()` serializa los campos nuevos sin romper.

### 3.5 Tests (`ml/tests/test_embeddings.py` — NUEVO)
No existe test del módulo de embeddings hoy. Crear:
- El registry contiene `granite-97m` con `backend="onnx"`, `dimension==384`.
- `LocalEmbedding("granite-97m")` construye `SentenceTransformer` con `backend="onnx"` y el `file_name` correcto (mockear `SentenceTransformer`, assert sobre kwargs).
- Retro-compat: un modelo torch (p.ej. `ml-mpnet`) sigue construyéndose **sin** `backend`.
- (Opcional, marcado `slow`/integration) smoke real: cargar el modelo y embeber un texto, assert `len(vector)==384`.

---

## 4. Migración / reindexado

Cambiar a `granite-97m` cambia la dimensión **768 → 384** → reindexado completo obligatorio.
- **LanceDB (local):** el schema fija la dimensión al crear la tabla; el reindex borra y recrea → OK.
- **Qdrant (shared):** valida dimensión contra la colección existente y **falla** si no coincide → hay que borrar/recrear la colección antes de reindexar.
- **Procedimiento seguro:** probar primero en un `DEVAI_STATE_DIR` + `DEVAI_LOCAL_DB_PATH` **aislados** (no el índice de producción), validar el recall real, y recién promover.

---

## 5. Riesgos y notas

- **AVX2 requerido:** `model_quint8_avx2.onnx` está cuantizado para CPU x86 con AVX2 (la mayoría de los equipos modernos lo tienen). En ARM no aplicaría → fallback a torch o a otra variante.
- **Peso del venv:** `optimum[onnxruntime]` agrega dependencias (cientos de MB). Aceptable para el servicio ML.
- **Versión de ST:** `backend="onnx"` exige ST >= 3.2; el mínimo declarado hoy (`>=2.2.0`) debe subir o instalaciones nuevas fallan.
- A diferencia del e5 cuantizado de terceros, el ONNX de IBM usa tokenizer HF estándar → **no** necesita `onnxruntime-extensions`.

---

## 6. Esfuerzo estimado

| Fase | Alcance | Esfuerzo |
|---|---|---|
| 1. Dependencias | pyproject + verificar instalación | bajo |
| 2. Registry + carga | `local.py` (ModelInfo, entrada, __init__, cache) | medio |
| 3. Descarga | asegurar subdir onnx/ | bajo |
| 4. Go (cosmético) | help text + verificar serialización RPC | bajo |
| 5. Tests | test_embeddings.py nuevo | medio |
| 6. Reindex + validación | state dir aislado, medir recall real | medio |

**Total: feature mediano**, mayormente concentrado en `local.py`. El grueso del valor (backend ONNX genérico) se reutiliza para cualquier modelo ONNX futuro.

---

## 7. Veredicto

**Vale la pena.** El beneficio no es marginal: indexado 6.3x más rápido, mejor calidad de búsqueda, y mitad de almacenamiento — y beneficia **cada** indexado y **cada** búsqueda, no solo este modelo. Además el backend queda como capacidad genérica del motor. El costo es un feature mediano + un reindexado one-time. ROI alto.
