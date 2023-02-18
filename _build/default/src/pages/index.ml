open Hyper

let html = [
  H.html ~attrs:[] [
    H.head ~attrs:[] [
      H.title ~attrs:[] [H.text "My website Hello There"];
    ];
    H.body ~attrs:[] [
      H.h1 ~attrs:[] [H.text "Welcome to my website"];
      H.p ~attrs:[] [H.text "Here is some content"];
    ];
  ]
]