#!/bin/bash
# Setup script for mp4_notes venvs
# Requires: Python 3.12, Homebrew (macOS)

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

echo "=== Setting up mp4_notes environments ==="

# Check Python 3.12
if ! command -v python3.12 &> /dev/null; then
    echo "Python 3.12 not found. Installing via Homebrew..."
    brew install python@3.12
fi

PYTHON312=$(which python3.12 || echo "/opt/homebrew/opt/python@3.12/bin/python3.12")

# === venv-gigaam (GigaAM ASR) ===
echo ""
echo "=== Setting up venv-gigaam ==="

if [ -L "venv-gigaam" ]; then
    echo "venv-gigaam is a symlink, removing..."
    rm venv-gigaam
fi

if [ ! -d "venv-gigaam" ]; then
    echo "Creating venv-gigaam..."
    $PYTHON312 -m venv venv-gigaam

    echo "Installing GigaAM dependencies..."
    ./venv-gigaam/bin/pip install --upgrade pip
    ./venv-gigaam/bin/pip install torch torchaudio
    ./venv-gigaam/bin/pip install soundfile sounddevice tqdm scikit-learn numpy speechbrain

    # Install GigaAM
    if [ -d "../GigaAM" ]; then
        echo "Installing GigaAM from ../GigaAM..."
        ./venv-gigaam/bin/pip install -e ../GigaAM
    else
        echo "GigaAM not found at ../GigaAM"
        echo "Please clone: git clone https://github.com/salute-developers/GigaAM.git ../GigaAM"
        echo "Then run: ./venv-gigaam/bin/pip install -e ../GigaAM"
    fi
else
    echo "venv-gigaam already exists, checking dependencies..."
    if ! ./venv-gigaam/bin/pip install --quiet speechbrain 2>&1; then
        echo "WARNING: Failed to install speechbrain into existing venv-gigaam."
        echo "Speaker identification will not work without it."
        echo "To fix: ./venv-gigaam/bin/pip install speechbrain"
    fi
fi

# === .env file ===
echo ""
echo "=== Checking .env ==="

if [ ! -f ".env" ]; then
    if [ -f ".env.example" ]; then
        echo "Creating .env from .env.example..."
        cp .env.example .env
        echo "Please edit .env and add your HF_TOKEN"
    else
        echo "Creating .env template..."
        echo "HF_TOKEN=hf_your_token_here" > .env
        echo "Please edit .env and add your HF_TOKEN"
    fi
else
    echo ".env exists"
fi

echo ""
echo "=== Setup complete ==="
echo ""
echo "Usage:"
echo "  ./venv-gigaam/bin/python mp4_notes.py live --list-devices"
echo "  ./venv-gigaam/bin/python mp4_notes.py live --system-device 0 --speaker-db speakers_ecapa.json"
