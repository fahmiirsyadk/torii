open Hyper

let bg_color = ("style", "background-color: pink; color: red;")

let html = [
  H.html ~attrs:[bg_color] [
    H.head ~attrs:[] [
      H.title ~attrs:[] [H.text "Pages About Here YAY"];
    ];
    H.body ~attrs:[] [
      H.h1 ~attrs:[] [H.text "This is about page WATCH"];
      H.p ~attrs:[] [H.text "Here is some about"];
    ];
  ]
]