open Core

module H = struct
  type attribute = string * string
  type node = Element of string * attribute list * node list | Text of string

  let elem tag ?(attrs=[]) children = Element (tag, attrs, children)

  let html ?attrs children = elem "html" ?attrs children
  let head ?attrs children = elem "head" ?attrs children
  let title ?attrs children = elem "title" ?attrs children
  let body ?attrs children = elem "body" ?attrs children
  let h1 ?attrs children = elem "h1" ?attrs children
  let p ?attrs children = elem "p" ?attrs children
  let text s = Text s
end

let write_to_file filename contents =
  Out_channel.with_file filename ~f:(fun chan -> Out_channel.output_string chan contents)

let rec render_node buf = function
  | H.Element (tag, attrs, children) ->
    Buffer.add_string buf ("<" ^ tag);
    List.iter attrs ~f:(fun (name, value) -> 
      Buffer.add_string buf (" " ^ name ^ "=\"" ^ value ^ "\""));
    Buffer.add_string buf ">";
    List.iter children ~f:(render_node buf);
    Buffer.add_string buf ("</" ^ tag ^ ">")
  | H.Text text -> Buffer.add_string buf text

let render ast =
  let buf = Buffer.create 4096 in
  List.iter ast ~f:(render_node buf);
  Buffer.contents buf