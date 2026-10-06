"""Plot measured gain and tone-fit residual from the Rust resampler test CSV."""
from pathlib import Path
import re

import matplotlib.pyplot as plt
import pandas as pd

HERE = Path(__file__).resolve().parent
DATA = pd.read_csv(HERE / "resampler-measurements.csv")
RATES = [(44_100, "44.1 → 48 kHz"), (48_000, "48 → 48 kHz (bypass)"), (96_000, "96 → 48 kHz")]
COLORS = {0: "#2563eb", 1: "#f97316"}

fig, axes = plt.subplots(2, 1, figsize=(24, 16), sharex=True, constrained_layout=True)
fig.suptitle("AscendCord capture resampler · measured equal-level sine sweep", fontsize=15)

for rate, label in RATES:
    subset = DATA[DATA.input_rate_hz == rate]
    for channel in (0, 1):
        rows = subset[subset.channel == channel]
        suffix = f"{label} · {'L' if channel == 0 else 'R'}"
        axes[0].semilogx(rows.frequency_hz, rows.gain_db, marker="o", ms=3,
                         lw=1.4, color=COLORS[channel],
                         label=suffix,
                         alpha=1 if channel == 0 else .58)
        axes[1].semilogx(rows.frequency_hz, rows.residual_dbc, marker=".", ms=3,
                         lw=1, color=COLORS[channel], alpha=.85 if channel == 0 else .48,
                         label=f"{label} · {'L' if channel == 0 else 'R'}")

axes[0].axhline(0, color="#334155", lw=.8)
axes[0].set_ylabel("Gain relative to input (dB · micro-detail view)")
axes[0].set_ylim(-0.000010, 0.000010)
axes[0].grid(True, which="both", alpha=.23)
axes[0].legend(ncol=3, fontsize=8, loc="lower left")
axes[0].text(.01, .98, "All samples also pass ±0.05 dB acceptance", transform=axes[0].transAxes,
             va="top", color="#16803c", fontsize=9)

axes[1].axhline(-75, color="#dc2626", ls="--", lw=1, label="residual limit: −75 dBc")
axes[1].set_ylabel("Non-tone residual (dBc)")
axes[1].set_xlabel("Input tone frequency (Hz, logarithmic scale)")
axes[1].set_ylim(-145, -70)
axes[1].grid(True, which="both", alpha=.23)
axes[1].legend(ncol=3, fontsize=8, loc="lower right")

out = HERE / "resampler-response.png"
fig.savefig(out, dpi=300)
svg_path = HERE / "resampler-response.svg"
fig.savefig(svg_path)
svg_path.write_text(
    re.sub(r"(?m)[ \t]+$", "", svg_path.read_text(encoding="utf-8")),
    encoding="utf-8",
)
print(out)
