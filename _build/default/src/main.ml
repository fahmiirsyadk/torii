open Core

let () =
  let start_time = Time.now () in
  let routes = Routes.routes in
  let write_route_to_file (route, ast) =
    let html_str = Hyper.render ast in
    let filename = route ^ ".html" in
    let%lwt () = Lwt_io.with_file ~mode:Lwt_io.output filename (fun chan -> Lwt_io.write chan html_str) in
    let end_time = Time.now () in
    printf "\nFile '%s' written in %d ms \n" filename (Time.diff end_time start_time |> Time.Span.to_ms |> Float.to_int);
    Lwt.return_unit
  in
  let result = Lwt_list.iter_p write_route_to_file routes in
  Lwt_main.run result