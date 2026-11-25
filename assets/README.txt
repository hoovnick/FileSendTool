icon.png   — the icon as a human-viewable 512x512 PNG.
icon.rgba  — the same icon at 64x64, raw RGBA (16384 bytes), compiled into the
             binary by src/icon.rs via include_bytes!. Stored raw so the binary
             needs no image decoder.

Regenerate both (needs Pillow):

    python - <<'EOF'
    from PIL import Image, ImageDraw
    S = 512
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle([0, 0, S - 1, S - 1], radius=int(S * 0.22), fill=(47, 129, 247, 255))
    d.polygon([(S // 2, int(S * 0.18)), (int(S * 0.80), int(S * 0.52)), (int(S * 0.63), int(S * 0.52)),
               (int(S * 0.63), int(S * 0.82)), (int(S * 0.37), int(S * 0.82)), (int(S * 0.37), int(S * 0.52)),
               (int(S * 0.20), int(S * 0.52))], fill=(255, 255, 255, 255))
    img.save("assets/icon.png")
    open("assets/icon.rgba", "wb").write(img.resize((64, 64), Image.LANCZOS).tobytes())
    EOF

If you change the size, update SIDE in src/icon.rs to match.
