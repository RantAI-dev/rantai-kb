# The layout detector behind the Xiaomi-OCR-0 recipe's table fallback.
#
# PP-DocLayoutV3 (a PaddleOCR model) through transformers, on the GPU. The
# model directory is mounted, not baked in: the weights are a download, and
# baking them would put a 130 MB checkpoint in every image layer.
FROM python:3.12-slim

ENV PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1
RUN apt-get update \
 && apt-get install -y --no-install-recommends libgl1 libglib2.0-0 curl \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY requirements.txt .
# torch and torchvision come from the CUDA index: the detector is a GPU model,
# and the plain PyPI wheel is not the one that matches a Blackwell card.
RUN pip install --no-cache-dir --extra-index-url https://download.pytorch.org/whl/cu130 \
    -r requirements.txt

COPY server.py .

ENV LAYOUT_HOST=0.0.0.0 LAYOUT_PORT=8193 LAYOUT_DEVICE=cuda:0
EXPOSE 8193
HEALTHCHECK --interval=10s --timeout=5s --retries=6 --start-period=120s \
  CMD curl -fsS http://127.0.0.1:8193/health || exit 1
USER nobody
CMD ["python", "server.py"]
