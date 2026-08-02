#!/usr/bin/env python3
"""El icono de k4term, dibujado y no pintado.

El diseño salió de una generación con imagegen (assets/referencia.png): un
squircle casi negro con el galón del intérprete en blanco y el cursor en el
verde de la casa. Lo que se guarda aquí no es aquel PNG sino las medidas que
tenía, porque un icono hay que servirlo desde 16 px hasta 512 y un bitmap
generado ni sale cuadrado —aquel medía 884x923— ni sobrevive al reescalado:
a tamaño de barra de tareas las curvas se deshacen.

Dibujarlo cuesta cuatro figuras y a cambio sale exacto en todos los tamaños,
con los colores del tema y no con los que el modelo creyó recordar.

    python3 assets/icono.py            # regenera assets/k4term-<lado>.png
"""

import pathlib

from PIL import Image, ImageDraw

#  Los del tema de k4, no los de la imagen generada: `superficie`, `tinta`,
#  `carril` y `verde` de ~/.local/state/k4/tema.json.
FONDO = (28, 28, 30, 255)
FILETE = (58, 58, 60, 255)
TINTA = (255, 255, 255, 255)
VERDE = (48, 209, 88, 255)

#  Se dibuja en grande y se baja: es la forma barata de tener bordes suaves
#  sin pelearse con el antialias de cada figura.
LIENZO = 4096
SUPER = 4

#  Proporciones medidas sobre la referencia, en tantos por uno del lado.
RADIO = 0.225
GALON_X0, GALON_APEX = 0.245, 0.570
GALON_Y0, GALON_Y1 = 0.260, 0.750
GROSOR = 0.070
CURSOR_X0, CURSOR_X1 = 0.675, 0.755
CURSOR_Y0, CURSOR_Y1 = 0.320, 0.680

LADOS = [512, 256, 128, 64, 48, 32, 24, 16]


def dibujar(lado):
    px = lambda v: v * lado
    img = Image.new("RGBA", (lado, lado), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    d.rounded_rectangle([0, 0, lado - 1, lado - 1], radius=px(RADIO), fill=FONDO)

    #  El filete interior: un pelo de luz en el borde para que el icono no se
    #  funda con un fondo oscuro. En la referencia casi no se ve, y así debe ser.
    margen = px(0.018)
    d.rounded_rectangle(
        [margen, margen, lado - 1 - margen, lado - 1 - margen],
        radius=px(RADIO) - margen,
        outline=FILETE,
        width=max(1, int(px(0.004))),
    )

    #  El galón, con las puntas redondas como en la referencia: dos trazos y
    #  tres círculos, que `line` con joint="curve" no redondea los extremos.
    grosor = px(GROSOR)
    r = grosor / 2
    puntos = [
        (px(GALON_X0), px(GALON_Y0)),
        (px(GALON_APEX), lado / 2),
        (px(GALON_X0), px(GALON_Y1)),
    ]
    d.line(puntos, fill=TINTA, width=int(round(grosor)))
    for x, y in puntos:
        d.ellipse([x - r, y - r, x + r, y + r], fill=TINTA)

    #  Y el cursor de bloque, a la derecha y sin redondear: es una celda de
    #  terminal, y las celdas son rectas.
    d.rectangle(
        [px(CURSOR_X0), px(CURSOR_Y0), px(CURSOR_X1), px(CURSOR_Y1)], fill=VERDE
    )
    return img


def main():
    aqui = pathlib.Path(__file__).resolve().parent
    grande = dibujar(LIENZO)
    for lado in LADOS:
        #  Del lienzo grande a cada tamaño de una vez: rebajar por pasos
        #  sucesivos emborrona, y a 16 px se nota.
        grande.resize((lado, lado), Image.LANCZOS).save(aqui / f"k4term-{lado}.png")
        print(f"assets/k4term-{lado}.png")


if __name__ == "__main__":
    main()
