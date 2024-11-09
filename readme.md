## pdfium-render api example

1. Make sure rust is installed on your machine
2. Inside `/pdfium` make sure you have a built file for pdfium based on your OS, libpdfium.dylib is for macOS only
3. `cargo run` will open a server on port `3000`
4. ```curl -X POST -F "file=@test.pdf" http://127.0.0.1:3000/process\?page\=2 --output test.png``` to test out a request