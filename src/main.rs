use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Query},
    http::StatusCode,
    response::IntoResponse,
    routing::post,
    Router,
};
use pdfium_render::prelude::*;
use regex::Regex;
use std::collections::HashMap;
use std::fmt::Write;
use std::io::Cursor;

#[derive(Clone)]
struct GeneratedRect {
    lx_pos: Vec<f32>,
    ly_pos: Vec<f32>,
    text: String,
    font_family: String,
    right: f32,
    font_size: f32,
}

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/process", post(process))
        .layer(DefaultBodyLimit::max(250 * 1024 * 1024));

    // Run the server
    // run our app with hyper, listening globally on port 3000
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap()
}

async fn process(
    Query(params): Query<HashMap<String, String>>,
    mut multipart: Multipart,
) -> impl IntoResponse {
    let re = Regex::new(r"/[\x00-\x08\x0B-\x0C\x0E-\x1F\x7F]|\r|\n/").unwrap();
    // Extract the page index from query parameters and validate
    // if no page is given then return page 0 image
    let page_index: usize = match params.get("page").and_then(|p| p.parse().ok()) {
        Some(page) => page,
        None => 0,
    };

    // Extract the PDF file from the multipart form
    let mut pdf_data: Option<Vec<u8>> = None;

    while let Some(field) = multipart.next_field().await.unwrap() {
        let data = field
            .bytes()
            .await
            .map_err(|_| StatusCode::BAD_REQUEST)
            .unwrap();
        pdf_data = Some(data.to_vec());
    }

    // get actual data
    let pdf_data = pdf_data.unwrap();

    // Create a new Pdfium instance for this request
    let pdfium = Pdfium::new(
        Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path("./pdfium")).unwrap(),
    );

    // Load the PDF document
    let document = pdfium.load_pdf_from_byte_vec(pdf_data, None).unwrap();

    // Get the specified page
    let page = document.pages().get(page_index as u16).unwrap();

    // Render the page as an image
    let render_config = PdfRenderConfig::new().set_target_width(800).set_format(PdfBitmapFormat::BGRA);

    let dynamic_image = page
        .render_with_config(&render_config)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
        .unwrap()
        .as_image() // Renders this page to an image::DynamicImage
        .into_rgb8(); // Converts to an RGB8 image

    let mut image_buffer = Vec::new();
    dynamic_image
        .write_to(&mut Cursor::new(&mut image_buffer), image::ImageFormat::Png)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
        .unwrap();

     // get page stuff
    let page_width = page.page_size().width().value;
    let page_height = page.page_size().width().value;

    let text = page.text().unwrap();
    let chars: PdfPageTextChars = text.chars();

    let mut groups: Vec<GeneratedRect> = Vec::new();
    let mut current_group: Option<GeneratedRect> = None;

    for (_index, char) in chars.iter().enumerate() {
        let curr = char.unicode_string().unwrap();
        let font_family = char.font_name();
        let char_origin_x = char.origin_x().unwrap().value;
        let char_origin_y = char.origin_y().unwrap().value;
        let loose_bounds = char.loose_bounds().unwrap();

        if char_origin_x < 0.0 || char_origin_y < 0.0 || re.is_match(&curr) {
            continue;
        }

        if let Some(ref mut unwrapped_current_group) = current_group {
            // Use `ref mut` to get a mutable reference to `current_group` directly
            let is_close_enough = (loose_bounds.left.value - unwrapped_current_group.right).abs()
                > loose_bounds.width().value + 5.0;
            let is_new_group =
                unwrapped_current_group.font_family != font_family || is_close_enough;

            if is_new_group {
                groups.push(unwrapped_current_group.clone());
                current_group = Some(GeneratedRect {
                    lx_pos: vec![char_origin_x],
                    ly_pos: vec![char_origin_y],
                    text: curr.clone(),
                    font_family: font_family.clone(),
                    right: loose_bounds.right.value,
                    font_size: loose_bounds.height().value,
                });
            } else {
                unwrapped_current_group.lx_pos.push(char_origin_x);
                unwrapped_current_group.ly_pos.push(char_origin_y);
                unwrapped_current_group.text.push_str(&curr);
                unwrapped_current_group.right = loose_bounds.right.value;
                unwrapped_current_group.font_size = unwrapped_current_group
                    .font_size
                    .max(loose_bounds.height().value);
            }
        } else {
            // Handle the case where `current_group` is `None`
            current_group = Some(GeneratedRect {
                lx_pos: vec![char_origin_x],
                ly_pos: vec![char_origin_y],
                text: curr.clone(),
                font_family: font_family.clone(),
                right: loose_bounds.right.value,
                font_size: loose_bounds.height().value,
            });
        }
    }

    if current_group.is_some() {
        groups.push(current_group.unwrap().clone());
    }

    let svg_content: String = generate_text_svg(page_width, page_height, groups);
    print!("{}", svg_content);


    let body = Body::from(image_buffer).into_response();
    return body;
}

fn generate_text_svg(page_width: f32, page_height: f32, rects: Vec<GeneratedRect>) -> String {
    if rects.is_empty() {
        return String::new();
    }

    let mut svg_content = format!(
        r#"<svg 
        xmlns="http://www.w3.org/2000/svg" 
        width="{page_width}" 
        height="{page_height}" 
        viewBox="0 0 {page_width} {page_height}" 
        style="font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; text-rendering: optimizeLegibility; shape-rendering: geometricPrecision"><title>text-layer</title>"#,
        page_width = page_width,
        page_height = page_height
    );

    for rect in rects {
        // Add text element with orientation-aware styling
        let _ = write!(
            svg_content,
            r#"<text 
            style="font-size:{font_size}pt; white-space: pre; text-rendering: geometricPrecision; dominant-baseline: hanging; font-weight: 400; letter-spacing: -0.01em; fill: rgb(230, 179, 179);">"#,
            font_size = rect.font_size,
        );

        let _ = write!(
            svg_content,
            r#"<tspan {primary_attr}="{primary_value}" {secondary_attr}="{secondary_value}">{text}</tspan></text>"#,
            primary_attr = "x",
            primary_value = rect
                .lx_pos
                .iter()
                .map(|num| num.to_string())
                .collect::<Vec<String>>()
                .join(" "),
            secondary_attr = "y",
            secondary_value = rect
                .ly_pos
                .iter()
                .map(|num| num.to_string())
                .collect::<Vec<String>>()
                .join(" "),
            text = rect.text
        );
    }

    svg_content.push_str("</svg>");
    svg_content
}
