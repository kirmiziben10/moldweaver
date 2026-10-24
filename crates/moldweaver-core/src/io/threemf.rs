//! 3MF (3D Manufacturing Format) reader and writer.
//!
//! The reader understands the core spec plus the production extension's
//! `p:path` component references (used by Bambu Studio / OrcaSlicer project
//! files). The writer produces a plain core-spec package with one object per
//! part and one build item per object, which Bambu Studio, OrcaSlicer and
//! PrusaSlicer all open as separate objects at the given plate positions.

use std::collections::HashMap;
use std::io::{Cursor, Read, Write};

use anyhow::{anyhow, bail, Context, Result};
use glam::{DMat4, DVec3, DVec4};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use crate::mesh::TriMesh;

const DEFAULT_MODEL_PATH: &str = "3D/3dmodel.model";

/// Read every build item of a 3MF package into one mesh (in millimetres).
pub fn read_3mf(bytes: &[u8]) -> Result<TriMesh> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).context("not a valid 3MF (zip) file")?;
    let root = root_model_path(&mut zip).unwrap_or_else(|| DEFAULT_MODEL_PATH.to_string());
    let mut files: HashMap<String, ModelFile> = HashMap::new();
    let root_model = load_model_file(&mut zip, &root, &mut files)?;
    let scale = root_model.unit_scale;
    let build: Vec<(String, DMat4)> = root_model.build.iter().map(|b| (b.object_id.clone(), b.transform)).collect();
    let mut out = TriMesh::default();
    if build.is_empty() {
        bail!("the 3MF has no build items");
    }
    for (id, transform) in &build {
        let mut depth = 0;
        emit_object(&mut zip, &mut files, &root, id, *transform, &mut out, &mut depth)?;
    }
    if scale != 1.0 {
        for p in &mut out.positions {
            *p *= scale;
        }
    }
    Ok(out)
}

fn root_model_path(zip: &mut zip::ZipArchive<Cursor<&[u8]>>) -> Option<String> {
    let mut rels = String::new();
    zip.by_name("_rels/.rels").ok()?.read_to_string(&mut rels).ok()?;
    let mut reader = Reader::from_str(&rels);
    loop {
        match reader.read_event() {
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) if e.local_name().as_ref() == b"Relationship" => {
                let ty = attr(&e, b"Type").unwrap_or_default();
                if ty.ends_with("/3dmodel") {
                    return attr(&e, b"Target").map(|t| t.trim_start_matches('/').to_string());
                }
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
    }
}

#[derive(Default)]
struct ModelFile {
    unit_scale: f64,
    objects: HashMap<String, Object>,
    build: Vec<BuildItem>,
}

#[derive(Default)]
struct Object {
    mesh: TriMesh,
    components: Vec<Component>,
}

struct Component {
    object_id: String,
    path: Option<String>,
    transform: DMat4,
}

struct BuildItem {
    object_id: String,
    transform: DMat4,
}

fn load_model_file<'a>(
    zip: &mut zip::ZipArchive<Cursor<&[u8]>>,
    path: &str,
    files: &'a mut HashMap<String, ModelFile>,
) -> Result<&'a ModelFile> {
    if !files.contains_key(path) {
        let mut xml = String::new();
        zip.by_name(path)
            .with_context(|| format!("3MF part {path} is missing"))?
            .read_to_string(&mut xml)
            .with_context(|| format!("reading {path}"))?;
        let parsed = parse_model_xml(&xml).with_context(|| format!("parsing {path}"))?;
        files.insert(path.to_string(), parsed);
    }
    Ok(&files[path])
}

fn emit_object(
    zip: &mut zip::ZipArchive<Cursor<&[u8]>>,
    files: &mut HashMap<String, ModelFile>,
    path: &str,
    id: &str,
    transform: DMat4,
    out: &mut TriMesh,
    depth: &mut usize,
) -> Result<()> {
    *depth += 1;
    if *depth > 64 {
        bail!("3MF component nesting is too deep (cycle?)");
    }
    let file = load_model_file(zip, path, files)?;
    let obj = file.objects.get(id).ok_or_else(|| anyhow!("3MF build references unknown object {id}"))?;
    let m = obj.mesh.transformed(&transform);
    let comps: Vec<(String, Option<String>, DMat4)> =
        obj.components.iter().map(|c| (c.object_id.clone(), c.path.clone(), c.transform)).collect();
    out.append(&m);
    for (cid, cpath, ct) in comps {
        let p = cpath.unwrap_or_else(|| path.to_string());
        emit_object(zip, files, &p, &cid, transform * ct, out, depth)?;
    }
    *depth -= 1;
    Ok(())
}

fn attr(e: &BytesStart, name: &[u8]) -> Option<String> {
    e.attributes().flatten().find_map(|a| {
        (a.key.local_name().as_ref() == name)
            .then(|| a.normalized_value(quick_xml::XmlVersion::Implicit1_0).map(|v| v.into_owned()).ok())?
    })
}

fn num(e: &BytesStart, name: &[u8]) -> Result<f64> {
    attr(e, name)
        .ok_or_else(|| anyhow!("missing attribute {}", String::from_utf8_lossy(name)))?
        .trim()
        .parse::<f64>()
        .map_err(|_| anyhow!("bad number in attribute {}", String::from_utf8_lossy(name)))
}

/// Parse a 3MF transform ("m00 m01 m02 m10 ... m32", row-vector convention).
fn parse_transform(s: Option<String>) -> Result<DMat4> {
    let Some(s) = s else { return Ok(DMat4::IDENTITY) };
    let v: Vec<f64> = s
        .split_whitespace()
        .map(|t| t.parse::<f64>())
        .collect::<Result<_, _>>()
        .map_err(|_| anyhow!("bad transform '{s}'"))?;
    if v.len() != 12 {
        bail!("transform must have 12 numbers, got {}", v.len());
    }
    // Column-major for glam: column j holds the coefficients of input axis j.
    Ok(DMat4::from_cols(
        DVec4::new(v[0], v[1], v[2], 0.0),
        DVec4::new(v[3], v[4], v[5], 0.0),
        DVec4::new(v[6], v[7], v[8], 0.0),
        DVec4::new(v[9], v[10], v[11], 1.0),
    ))
}

fn parse_model_xml(xml: &str) -> Result<ModelFile> {
    let mut reader = Reader::from_str(xml);
    let mut file = ModelFile { unit_scale: 1.0, ..Default::default() };
    let mut current: Option<(String, Object)> = None;
    let mut in_build = false;
    loop {
        let ev = reader.read_event().context("malformed XML")?;
        let (e, is_empty) = match &ev {
            Event::Start(e) => (e, false),
            Event::Empty(e) => (e, true),
            Event::End(e) => {
                match e.local_name().as_ref() {
                    b"object" => {
                        if let Some((id, obj)) = current.take() {
                            file.objects.insert(id, obj);
                        }
                    }
                    b"build" => in_build = false,
                    _ => {}
                }
                continue;
            }
            Event::Eof => break,
            _ => continue,
        };
        match e.local_name().as_ref() {
            b"model" => {
                file.unit_scale = match attr(e, b"unit").as_deref().unwrap_or("millimeter") {
                    "micron" => 0.001,
                    "millimeter" => 1.0,
                    "centimeter" => 10.0,
                    "inch" => 25.4,
                    "foot" => 304.8,
                    "meter" => 1000.0,
                    other => bail!("unknown 3MF unit '{other}'"),
                };
            }
            b"object" => {
                let id = attr(e, b"id").ok_or_else(|| anyhow!("object without id"))?;
                if is_empty {
                    file.objects.insert(id, Object::default());
                } else {
                    current = Some((id, Object::default()));
                }
            }
            b"vertex" => {
                if let Some((_, obj)) = &mut current {
                    obj.mesh.positions.push(DVec3::new(num(e, b"x")?, num(e, b"y")?, num(e, b"z")?));
                }
            }
            b"triangle" => {
                if let Some((_, obj)) = &mut current {
                    let n = obj.mesh.positions.len() as f64;
                    let mut t = [0u32; 3];
                    for (k, key) in [b"v1", b"v2", b"v3"].iter().enumerate() {
                        let v = num(e, *key)?;
                        if v < 0.0 || v >= n || v.fract() != 0.0 {
                            bail!("triangle index {v} out of range");
                        }
                        t[k] = v as u32;
                    }
                    obj.mesh.triangles.push(t);
                }
            }
            b"component" => {
                if let Some((_, obj)) = &mut current {
                    obj.components.push(Component {
                        object_id: attr(e, b"objectid").ok_or_else(|| anyhow!("component without objectid"))?,
                        path: attr(e, b"path").map(|p| p.trim_start_matches('/').to_string()),
                        transform: parse_transform(attr(e, b"transform"))?,
                    });
                }
            }
            b"build" => in_build = !is_empty,
            b"item" if in_build => {
                file.build.push(BuildItem {
                    object_id: attr(e, b"objectid").ok_or_else(|| anyhow!("build item without objectid"))?,
                    transform: parse_transform(attr(e, b"transform"))?,
                });
            }
            _ => {}
        }
    }
    Ok(file)
}

/// One object to write into a 3MF package.
pub struct ExportObject<'a> {
    pub name: &'a str,
    /// Mesh in its final print orientation, in millimetres.
    pub mesh: &'a TriMesh,
    /// Translation applied through the build item (plate position).
    pub translation: DVec3,
}

/// Write a core-spec 3MF package.
pub fn write_3mf(objects: &[ExportObject], title: &str) -> Result<Vec<u8>> {
    use std::fmt::Write as _;
    let mut model = String::with_capacity(1 << 20);
    model.push_str(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <model unit=\"millimeter\" xml:lang=\"en-US\" xmlns=\"http://schemas.microsoft.com/3dmanufacturing/core/2015/02\">\n",
    );
    let _ = writeln!(model, " <metadata name=\"Title\">{}</metadata>", xml_escape(title));
    model.push_str(" <metadata name=\"Application\">MoldWeaver</metadata>\n <resources>\n");
    for (i, obj) in objects.iter().enumerate() {
        let _ = writeln!(
            model,
            "  <object id=\"{}\" name=\"{}\" type=\"model\">\n   <mesh>\n    <vertices>",
            i + 1,
            xml_escape(obj.name)
        );
        for p in &obj.mesh.positions {
            let _ = writeln!(model, "     <vertex x=\"{}\" y=\"{}\" z=\"{}\"/>", p.x as f32, p.y as f32, p.z as f32);
        }
        model.push_str("    </vertices>\n    <triangles>\n");
        for t in &obj.mesh.triangles {
            let _ = writeln!(model, "     <triangle v1=\"{}\" v2=\"{}\" v3=\"{}\"/>", t[0], t[1], t[2]);
        }
        model.push_str("    </triangles>\n   </mesh>\n  </object>\n");
    }
    model.push_str(" </resources>\n <build>\n");
    for (i, obj) in objects.iter().enumerate() {
        let t = obj.translation;
        let _ = writeln!(
            model,
            "  <item objectid=\"{}\" transform=\"1 0 0 0 1 0 0 0 1 {} {} {}\" printable=\"1\"/>",
            i + 1,
            t.x as f32,
            t.y as f32,
            t.z as f32
        );
    }
    model.push_str(" </build>\n</model>\n");

    let content_types = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\n \
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\n \
<Default Extension=\"model\" ContentType=\"application/vnd.ms-package.3dmanufacturing-3dmodel+xml\"/>\n\
</Types>\n";
    let rels = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\n \
<Relationship Target=\"/3D/3dmodel.model\" Id=\"rel0\" Type=\"http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel\"/>\n\
</Relationships>\n";

    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("[Content_Types].xml", opts)?;
    zip.write_all(content_types.as_bytes())?;
    zip.start_file("_rels/.rels", opts)?;
    zip.write_all(rels.as_bytes())?;
    zip.start_file(DEFAULT_MODEL_PATH, opts)?;
    zip.write_all(model.as_bytes())?;
    Ok(zip.finish()?.into_inner())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Light-weight structural check of a 3MF package written by anyone: the
/// zip opens, the required parts exist, all XML is well formed and every
/// object mesh references valid vertices. Returns per-object info.
pub fn validate_3mf(bytes: &[u8]) -> Result<Vec<ValidatedObject>> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).context("not a zip archive")?;
    for required in ["[Content_Types].xml", "_rels/.rels"] {
        let mut s = String::new();
        zip.by_name(required).with_context(|| format!("missing {required}"))?.read_to_string(&mut s)?;
        check_well_formed(&s).with_context(|| format!("{required} is not well-formed XML"))?;
    }
    let root = root_model_path(&mut zip).ok_or_else(|| anyhow!("no 3D model relationship"))?;
    let mut xml = String::new();
    zip.by_name(&root)?.read_to_string(&mut xml)?;
    check_well_formed(&xml).context("model XML is not well-formed")?;
    let file = parse_model_xml(&xml)?;
    let mut out = Vec::new();
    for item in &file.build {
        let obj = file
            .objects
            .get(&item.object_id)
            .ok_or_else(|| anyhow!("build item references missing object {}", item.object_id))?;
        let mesh = obj.mesh.transformed(&item.transform);
        out.push(ValidatedObject { name: object_name(&xml, &item.object_id), bounds: mesh.bounds(), mesh });
    }
    Ok(out)
}

pub struct ValidatedObject {
    pub name: String,
    pub mesh: TriMesh,
    pub bounds: crate::mesh::Aabb,
}

fn object_name(xml: &str, id: &str) -> String {
    let mut reader = Reader::from_str(xml);
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e))
                if e.local_name().as_ref() == b"object" && attr(&e, b"id").as_deref() == Some(id) =>
            {
                return attr(&e, b"name").unwrap_or_default();
            }
            Ok(Event::Eof) | Err(_) => return String::new(),
            _ => {}
        }
    }
}

fn check_well_formed(xml: &str) -> Result<()> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().check_end_names = true;
    let mut depth: i64 = 0;
    loop {
        match reader.read_event()? {
            Event::Start(_) => depth += 1,
            Event::End(_) => depth -= 1,
            Event::Eof => break,
            _ => {}
        }
        if depth < 0 {
            bail!("unbalanced tags");
        }
    }
    if depth != 0 {
        bail!("unclosed tags");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::primitives::cuboid;

    #[test]
    fn write_then_read_and_validate() {
        let a = cuboid(DVec3::ZERO, DVec3::splat(10.0));
        let b = cuboid(DVec3::ZERO, DVec3::new(5.0, 5.0, 20.0));
        let bytes = write_3mf(
            &[
                ExportObject { name: "A & <B>", mesh: &a, translation: DVec3::new(10.0, 10.0, 0.0) },
                ExportObject { name: "second", mesh: &b, translation: DVec3::new(30.0, 10.0, 0.0) },
            ],
            "test",
        )
        .unwrap();
        let objs = validate_3mf(&bytes).unwrap();
        assert_eq!(objs.len(), 2);
        assert_eq!(objs[0].name, "A & <B>");
        assert_eq!(objs[1].bounds.min, DVec3::new(30.0, 10.0, 0.0));
        let merged = read_3mf(&bytes).unwrap().welded(1e-6);
        assert_eq!(merged.triangles.len(), 24);
        assert!((merged.signed_volume() - 1500.0).abs() < 1e-3);
    }

    #[test]
    fn components_transforms_and_units() {
        // A cube object referenced twice through components, in centimetres.
        let model = r#"<?xml version="1.0"?>
<model unit="centimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">
 <resources>
  <object id="1" type="model"><mesh><vertices>
   <vertex x="0" y="0" z="0"/><vertex x="1" y="0" z="0"/><vertex x="0" y="1" z="0"/><vertex x="0" y="0" z="1"/>
  </vertices><triangles>
   <triangle v1="0" v2="2" v3="1"/><triangle v1="0" v2="1" v3="3"/><triangle v1="0" v2="3" v3="2"/><triangle v1="1" v2="2" v3="3"/>
  </triangles></mesh></object>
  <object id="2" type="model"><components>
   <component objectid="1"/>
   <component objectid="1" transform="1 0 0 0 1 0 0 0 1 5 0 0"/>
  </components></object>
 </resources>
 <build><item objectid="2" transform="1 0 0 0 1 0 0 0 1 0 0 2"/></build>
</model>"#;
        let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Target="/3D/3dmodel.model" Id="r" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/></Relationships>"#;
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let o = zip::write::SimpleFileOptions::default();
        zip.start_file("_rels/.rels", o).unwrap();
        zip.write_all(rels.as_bytes()).unwrap();
        zip.start_file("3D/3dmodel.model", o).unwrap();
        zip.write_all(model.as_bytes()).unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        let m = read_3mf(&bytes).unwrap();
        assert_eq!(m.triangles.len(), 8);
        let b = m.bounds();
        assert!((b.min - DVec3::new(0.0, 0.0, 20.0)).length() < 1e-9);
        assert!((b.max - DVec3::new(60.0, 10.0, 30.0)).length() < 1e-9);
        assert!((m.signed_volume() - 2.0 * 1000.0 / 6.0).abs() < 1e-6);
    }
}
