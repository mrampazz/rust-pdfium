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

struct PageImage {
    scale: f32,
    buffer: Vec<u8>,
}

// TODO: do we need the full text as a string?
struct PagePayload {
    svg_text: String,
    images: Vec<PageImage>,
}

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/process", post(process_pdf))
        .layer(DefaultBodyLimit::max(250 * 1024 * 1024));

    // Run the server
    // run our app with hyper, listening globally on port 1234
    let listener = tokio::net::TcpListener::bind("0.0.0.0:1234").await.unwrap();
    axum::serve(listener, app).await.unwrap()
}

async fn process_pdf(
    Query(params): Query<HashMap<String, String>>,
    mut multipart: Multipart,
) -> impl IntoResponse {
    // Extract the boolean which represents if we are dealing with a main book or with an answer book
    let is_answer_book: bool = match params
        .get("answer_book")
        .and_then(|p| p.parse::<usize>().ok())
    {
        Some(value) => value != 0,
        None => false,
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
    let pdf_data = pdf_data.unwrap();

    // Create a new Pdfium instance for this request
    let pdfium = Pdfium::new(
        Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path("./pdfium"))
            .map_err(|_| StatusCode::BAD_REQUEST)
            .unwrap(),
    );

    // Load the PDF document
    let document = pdfium
        .load_pdf_from_byte_vec(pdf_data, None)
        .map_err(|_| StatusCode::BAD_REQUEST)
        .unwrap();

    let mut pages_payload: Vec<PagePayload> = Vec::new();

    // Iterate over the document's pages to parse the text & generate the images
    for (_u, page) in document.pages().iter().enumerate() {
        let page_ref = &page;
        // Get page size info
        let page_width = page_ref.width().value;
        let page_height = page_ref.height().value;

        // Parse the page for the text & generate svg string
        let text_group_rects = extract_page_text_groups(page_ref, page_height);
        let svg_text = get_string_from_rects(page_width, page_height, text_group_rects);

        // Generate the images
        let page_images = generate_page_images(page_ref, page_width, page_height, is_answer_book);

        pages_payload.push(PagePayload {
            svg_text,
            images: page_images,
        })
    }

    print!("{}", pages_payload[0].svg_text);

    // Send over payload
    // TODO: figure out how to send the actual payload
    // let body = Body::from(pages_payload[0].svg_text.clone()).into_response();
    let body = Body::from(pages_payload[0].images[2].buffer.clone()).into_response();
    return body;
}

// returns the svg string from the generated text rects
fn get_string_from_rects(page_width: f32, page_height: f32, rects: Vec<GeneratedRect>) -> String {
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
            r#"<tspan x="{primary_value}" y="{secondary_value}">{text}</tspan></text>"#,
            primary_value = rect
                .lx_pos
                .iter()
                .map(|num| num.to_string())
                .collect::<Vec<String>>()
                .join(" "),
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

// calculated manually by iterating over the chars to get their absolute origin and grouped by closeness & font size
// returns the text boxes for this page
// TODO: for certain text it gets cut off when printing it
fn extract_page_text_groups(page: &PdfPage<'_>, page_height: f32) -> Vec<GeneratedRect> {
    let re = Regex::new(r"/[\x00-\x08\x0B-\x0C\x0E-\x1F\x7F]|\r|\n/").unwrap();

    let text = page.text().unwrap();
    let chars: PdfPageTextChars = text.chars();

    let mut groups: Vec<GeneratedRect> = Vec::new();
    let mut current_group: Option<GeneratedRect> = None;

    for (_index, char) in chars.iter().enumerate() {
        let curr = char.unicode_string().unwrap();
        let font_family = char.font_name();
        let char_origin_x = char.origin_x().unwrap().value;
        let mut char_origin_y = char.origin_y().unwrap().value;
        let loose_bounds = char.loose_bounds().unwrap();

        // fix up y coordinates due to different origin
        char_origin_y = page_height - char_origin_y;

        // skip the iteration if the char is outside the page, if the current char is not printable or if its height is 0.0
        if char_origin_x < 0.0
            || char_origin_y < 0.0
            || re.is_match(&curr)
            || loose_bounds.height().value == 0.0
        {
            continue;
        }

        // Use `ref mut` to get a mutable reference to `current_group` directly
        if let Some(ref mut unwrapped_current_group) = current_group {
            let is_close_enough = (loose_bounds.left.value - unwrapped_current_group.right).abs()
                > loose_bounds.width().value + 5.0;
            let is_new_group =
                unwrapped_current_group.font_family != font_family || is_close_enough;

            if is_new_group {
                groups.push(unwrapped_current_group.clone());
                current_group = Some(GeneratedRect {
                    lx_pos: vec![char_origin_x],
                    ly_pos: vec![char_origin_y - loose_bounds.height().value],
                    text: curr.clone(),
                    font_family: font_family.clone(),
                    right: loose_bounds.right.value,
                    font_size: loose_bounds.height().value,
                });
            } else {
                unwrapped_current_group.font_size = unwrapped_current_group
                    .font_size
                    .max(loose_bounds.height().value);
                unwrapped_current_group.lx_pos.push(char_origin_x);
                unwrapped_current_group
                    .ly_pos
                    .push(char_origin_y - unwrapped_current_group.font_size);
                unwrapped_current_group.text.push_str(&curr);
                unwrapped_current_group.right = loose_bounds.right.value;
            }
        } else {
            // Handle the case where `current_group` is `None`
            current_group = Some(GeneratedRect {
                lx_pos: vec![char_origin_x],
                ly_pos: vec![char_origin_y - loose_bounds.height().value],
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
    return groups;
}

// function to return the images as buffers at specific scales
fn generate_page_images(
    page: &PdfPage<'_>,
    page_width: f32,
    page_height: f32,
    with_transparency: bool,
) -> Vec<PageImage> {
    let mut result: Vec<PageImage> = Vec::new();
    let mut color: PdfColor = PdfColor::WHITE;
    if with_transparency {
        color = color.with_alpha(0);
    }
    // TODO: define which scales you want
    let scales: Vec<f32> = vec![0.25, 0.5, 1.0, 1.5, 2.0];
    for (_i, scale) in scales.iter().enumerate() {
        let render_config = PdfRenderConfig::new()
            .set_format(PdfBitmapFormat::BGRA)
            .set_reverse_byte_order(true)
            .set_clear_color(color)
            .set_target_size((page_width * scale) as i32, (page_height * scale) as i32);

        let dynamic_image = page
            .render_with_config(&render_config)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
            .unwrap()
            .as_image() // Renders this page to an image::DynamicImage
            .into_rgba8();
        let mut image_buffer = Vec::new();
        dynamic_image
            .write_to(&mut Cursor::new(&mut image_buffer), image::ImageFormat::Png)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
            .unwrap();
        result.push(PageImage {
            scale: *scale,
            buffer: image_buffer,
        });
    }
    return result;
}
