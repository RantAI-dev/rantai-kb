# The layout detector behind the Xiaomi-OCR-0 recipe's table fallback.
#
# PP-DocLayoutV3 (a PaddleOCR model, Apache-2.0) through transformers. It runs on
# the CPU by default: on the same GPU as the vLLM OCR server it crashed or hung the
# engine (docs/kb-tiers.md). The ~130 MB checkpoint is baked in so the container
# starts offline; LAYOUT_MODEL_DIR can still point at a mounted copy.
#
#   docker build --build-arg TORCH_INDEX=https://download.pytorch.org/whl/cu130 \
#       --build-arg LAYOUT_DEVICE=cuda:0 ...   # a GPU build, for a second card
FROM python:3.12-slim

ENV PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1
RUN apt-get update \
 && apt-get install -y --no-install-recommends libgl1 libglib2.0-0 curl \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY requirements.txt .
# CPU wheels by default (small image, no CUDA runtime); TORCH_INDEX picks a CUDA index.
ARG TORCH_INDEX=https://download.pytorch.org/whl/cpu
RUN pip install --no-cache-dir --extra-index-url "$TORCH_INDEX" -r requirements.txt

ARG LAYOUT_MODEL_ID=PaddlePaddle/PP-DocLayoutV3_safetensors
RUN python -c "from huggingface_hub import snapshot_download; snapshot_download('$LAYOUT_MODEL_ID', local_dir='/app/model')"

COPY server.py .

ARG LAYOUT_DEVICE=cpu
ENV LAYOUT_HOST=0.0.0.0 LAYOUT_PORT=8193 LAYOUT_DEVICE=${LAYOUT_DEVICE} LAYOUT_MODEL_DIR=/app/model
EXPOSE 8193
HEALTHCHECK --interval=10s --timeout=5s --retries=6 --start-period=120s \
  CMD curl -fsS http://127.0.0.1:8193/health || exit 1
USER nobody
CMD ["python", "server.py"]
