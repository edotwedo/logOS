#!/usr/bin/env python3
"""
Procedural Dreamscape Raymarcher & Aesthetic Shader for logOS
Computes volumetric glowing SDFs, iridescent chromatic dispersion, and harmonic nebula waves.
"""

import math
import numpy as np
from PIL import Image

WIDTH = 1920
HEIGHT = 1080

print(f"Generating logOS procedural dreamscape ({WIDTH}x{HEIGHT})...")

# Coordinate grids normalized [-1, 1] with aspect ratio correction
y, x = np.mgrid[0:HEIGHT, 0:WIDTH]
uv_x = (x - WIDTH * 0.5) / (HEIGHT * 0.5)
uv_y = (HEIGHT * 0.5 - y) / (HEIGHT * 0.5)

# Radial distance and angle
r = np.sqrt(uv_x**2 + uv_y**2)
theta = np.arctan2(uv_y, uv_x)

# 1. Harmonic Nebula Wavefield (multiscale sines & twisting spirals)
spiral = theta * 3.0 + np.sin(r * 4.0 - 0.8) * 1.5
wave1 = np.sin(spiral * 2.0 + r * 6.0)
wave2 = np.cos(spiral * 3.0 - r * 8.0)
nebula = (wave1 * 0.5 + wave2 * 0.5) * np.exp(-r * 0.8)

# 2. Glowing Core SDF (pulsating geometric singularity with soft falloff)
core_dist = np.abs(r - 0.45) - 0.04
core_glow = 0.08 / (np.abs(core_dist) + 0.04)

inner_orb = 0.05 / (r + 0.08)

# 3. Celestial Bloom / God Rays
rays = (np.sin(theta * 12.0 + np.cos(r * 5.0)) * 0.5 + 0.5) ** 3
ray_intensity = rays * np.exp(-r * 1.2) * 0.7

# 4. Color Palette Synthesis:
# Base background: Deep midnight void (0.02, 0.01, 0.05)
# Mid tones: Electric violet & magenta
# Highlights: Luminous cyan & prismatic teal

# Red channel
red = (
    0.04 * (1.0 - r * 0.4)
    + 0.55 * np.maximum(0.0, nebula)
    + 0.85 * core_glow * 0.6
    + 0.95 * inner_orb * 0.9
    + 0.25 * ray_intensity
)

# Green channel
green = (
    0.02 * (1.0 - r * 0.5)
    + 0.25 * np.maximum(0.0, -nebula)
    + 0.65 * core_glow * 0.7
    + 0.80 * inner_orb * 0.6
    + 0.55 * ray_intensity * 0.8
)

# Blue channel
blue = (
    0.15 * (1.0 - r * 0.3)
    + 0.90 * np.abs(nebula) * 0.8
    + 1.20 * core_glow * 0.9
    + 1.40 * inner_orb
    + 0.90 * ray_intensity
)

# Vignette and tone mapping
vignette = np.clip(1.3 - r * 0.65, 0.0, 1.0)
red = np.clip(red * vignette, 0.0, 1.0)
green = np.clip(green * vignette, 0.0, 1.0)
blue = np.clip(blue * vignette, 0.0, 1.0)

# Smooth tone curve
rgb = np.stack([red, green, blue], axis=-1)
rgb = np.power(rgb, 0.85) # slight gamma push for eye candy pop
img_bytes = (rgb * 255.0).astype(np.uint8)

img = Image.fromarray(img_bytes, mode="RGB")
output_path = "/home/edotwedo/workspace/logOS/dreamscape_preview.png"
img.save(output_path, quality=95)
print(f"Saved dreamscape preview to {output_path}")
