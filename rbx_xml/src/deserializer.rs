use std::{
    io::Read,
    iter::Peekable,
    collections::HashMap,
};

use log::trace;
use rbx_tree::{RbxTree, RbxId, RbxInstanceProperties, RbxValue};

use xml::{
    ParserConfig,
    EventReader,
    reader::{self, XmlEvent},
};

use crate::{
    types::{
        deserialize_bool,
        deserialize_string,
        deserialize_cframe,
        deserialize_color3,
        deserialize_color3uint8,
        deserialize_vector2,
        deserialize_vector2int16,
        deserialize_vector3,
        deserialize_vector3int16,
    },
};

/// Indicates an error trying to parse an rbxmx or rbxlx document
#[derive(Debug, Clone, PartialEq)]
pub enum DecodeError {
    XmlError(reader::Error),
    ParseFloatError(std::num::ParseFloatError),
    ParseIntError(std::num::ParseIntError),
    Message(&'static str),
    MalformedDocument,
}

impl From<reader::Error> for DecodeError {
    fn from(error: reader::Error) -> DecodeError {
        DecodeError::XmlError(error)
    }
}

impl From<std::num::ParseFloatError> for DecodeError {
    fn from(error: std::num::ParseFloatError) -> DecodeError {
        DecodeError::ParseFloatError(error)
    }
}

impl From<std::num::ParseIntError> for DecodeError {
    fn from(error: std::num::ParseIntError) -> DecodeError {
        DecodeError::ParseIntError(error)
    }
}

/// A utility method to decode an XML-format model from a string.
pub fn decode_str(tree: &mut RbxTree, parent_id: RbxId, source: &str) -> Result<(), DecodeError> {
    decode(tree, parent_id, source.as_bytes())
}

/// Decodes source from the given buffer into the instance in the given tree.
///
/// Roblox model files can contain multiple instances at the top level. This
/// happens in the case of places as well as Studio users choosing multiple
/// objects when saving a model file.
pub fn decode<R: Read>(tree: &mut RbxTree, parent_id: RbxId, source: R) -> Result<(), DecodeError> {
    let reader = ParserConfig::new()
        .coalesce_characters(true)
        .cdata_to_characters(true)
        .ignore_comments(true)
        .create_reader(source);

    let mut iterator = EventIterator::from_reader(reader);
    let mut state = ParseState::new(tree);

    deserialize_root(&mut iterator, &mut state, parent_id)
}

pub struct EventIterator<R: Read> {
    inner: Peekable<reader::Events<R>>,
}

impl<R: Read> EventIterator<R> {
    pub fn peek(&mut self) -> Option<&<Self as Iterator>::Item> {
        self.inner.peek()
    }

    fn from_reader(reader: EventReader<R>) -> EventIterator<R> {
        EventIterator {
            inner: reader.into_iter().peekable(),
        }
    }

    /// Reads a tag completely and returns its text content. This is intended
    /// for parsing simple tags where we don't care about the attributes or
    /// children, only the text value, for Vector3s and such, which are encoded
    /// like:
    ///
    /// <Vector3>
    ///     <X>0</X>
    ///     <Y>0</Y>
    ///     <Z>0</Z>
    /// </Vector3>
    pub fn read_tag_contents(&mut self, expected_name: &str) -> Result<String, DecodeError> {
        read_event!(self, XmlEvent::StartElement { name, .. } => {
            if name.local_name != expected_name {
                return Err(DecodeError::Message("got wrong tag name"));
            }
        });

        let contents = read_event!(self, XmlEvent::Characters(content) => content);
        read_event!(self, XmlEvent::EndElement { .. } => {});

        Ok(contents)
    }
}

impl<R: Read> Iterator for EventIterator<R> {
    type Item = reader::Result<XmlEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next()
    }
}

struct ParseState<'a> {
    referents: HashMap<String, RbxId>,
    metadata: HashMap<String, String>,
    tree: &'a mut RbxTree,
}

impl<'a> ParseState<'a> {
    fn new(tree: &mut RbxTree) -> ParseState {
        ParseState {
            referents: HashMap::new(),
            metadata: HashMap::new(),
            tree,
        }
    }
}

fn deserialize_root<R: Read>(reader: &mut EventIterator<R>, state: &mut ParseState, parent_id: RbxId) -> Result<(), DecodeError> {
    match reader.next().ok_or(DecodeError::MalformedDocument)?? {
        XmlEvent::StartDocument { .. } => {},
        _ => return Err(DecodeError::MalformedDocument),
    }

    read_event!(reader, XmlEvent::StartElement { name, attributes, .. } => {
        if name.local_name != "roblox" {
            return Err(DecodeError::Message("Missing <roblox>"));
        }

        let mut found_version = false;
        for attribute in &attributes {
            if attribute.name.local_name == "version" {
                found_version = true;

                if attribute.value != "4" {
                    return Err(DecodeError::Message("Not version 4"));
                }
            }
        }

        if !found_version {
            return Err(DecodeError::Message("No version field"));
        }
    });

    loop {
        match reader.peek().ok_or(DecodeError::MalformedDocument)? {
            Ok(XmlEvent::StartElement { name, .. }) => {
                match name.local_name.as_str() {
                    "Item" => {
                        deserialize_instance(reader, state, parent_id)?;
                    },
                    "External" => {
                        // This tag is always meaningless, there's nothing to do
                        // here except skip it.
                        eat_unknown_tag(reader)?;
                    },
                    "Meta" => {
                        // TODO: Actually parse metadata
                        deserialize_metadata(reader, state)?;
                    },
                    _ => return Err(DecodeError::MalformedDocument),
                }
            },
            Ok(XmlEvent::EndElement { name, .. }) => {
                if name.local_name == "roblox" {
                    break;
                } else {
                    return Err(DecodeError::Message("Unexpected closing tag"));
                }
            },
            Ok(XmlEvent::EndDocument) => break,
            Ok(_) => {
                let _ = reader.next();
            },
            Err(_) => {
                match reader.next().unwrap() {
                    Err(e) => return Err(e.into()),
                    Ok(_) => unreachable!(),
                }
            },
        }
    }

    Ok(())
}

fn deserialize_metadata<R: Read>(reader: &mut EventIterator<R>, state: &mut ParseState) -> Result<(), DecodeError> {
    // TODO: Strongly type metadata instead?

    let name = read_event!(reader, XmlEvent::StartElement { name, mut attributes, .. } => {
        assert_eq!(name.local_name, "Meta");

        let mut name = None;

        for attribute in attributes.drain(..) {
            match attribute.name.local_name.as_str() {
                "name" => name = Some(attribute.value),
                _ => {},
            }
        }

        name.ok_or(DecodeError::MalformedDocument)?
    });

    let value = read_event!(reader, XmlEvent::Characters(value) => value);

    read_event!(reader, XmlEvent::EndElement { name, .. } => {
        if name.local_name != "Meta" {
            return Err(DecodeError::MalformedDocument);
        }
    });

    trace!("Metadata: {} = {}", name, value);

    state.metadata.insert(name, value);
    Ok(())
}

fn deserialize_instance<R: Read>(reader: &mut EventIterator<R>, state: &mut ParseState, parent_id: RbxId) -> Result<(), DecodeError> {
    let (class_name, referent) = read_event!(reader, XmlEvent::StartElement { name, mut attributes, .. } => {
        assert_eq!(name.local_name, "Item");

        let mut class = None;
        let mut referent = None;

        for attribute in attributes.drain(..) {
            match attribute.name.local_name.as_str() {
                "class" => class = Some(attribute.value),
                "referent" => referent = Some(attribute.value),
                _ => {},
            }
        }

        let class = class.ok_or(DecodeError::Message("Missing 'class'"))?;

        (class, referent)
    });

    trace!("Class {} with referent {:?}", class_name, referent);

    let instance_props = RbxInstanceProperties {
        class_name: String::new(),
        name: String::new(),
        properties: HashMap::new(),
    };

    let instance_id = state.tree.insert_instance(instance_props, parent_id);

    if let Some(referent) = referent {
        state.referents.insert(referent, instance_id);
    }

    // we have to collect properties in order to create the instance
    // name will be captured in this map and extracted later; XML doesn't store it separately
    let mut properties: HashMap<String, RbxValue> = HashMap::new();

    let mut depth = 1;

    loop {
        match reader.peek().ok_or(DecodeError::MalformedDocument)? {
            Ok(XmlEvent::StartElement { name, .. }) => {
                depth += 1;

                match name.local_name.as_str() {
                    "Properties" => {
                        deserialize_properties(reader, &mut properties)?;
                    },
                    "Item" => {
                        deserialize_instance(reader, state, instance_id)?;
                    }
                    _ => return Err(DecodeError::MalformedDocument),
                }
            },
            Ok(XmlEvent::EndElement { .. }) => {
                reader.next();
                depth -= 1;

                if depth <= 1 {
                    break;
                }
            },
            Ok(XmlEvent::Whitespace(_)) => {
                reader.next();
            },
            _ => {},
        }
    }

    let instance_name = match properties.remove("Name") {
        Some(value) => match value {
            RbxValue::String { value } => value,
            _ => return Err(DecodeError::Message("Name must be a string")),
        },
        None => class_name.clone(),
    };

    let instance = state.tree.get_instance_mut(instance_id).unwrap();
    instance.class_name = class_name;
    instance.name = instance_name;
    instance.properties = properties;

    Ok(())
}

fn deserialize_properties<R: Read>(reader: &mut EventIterator<R>, props: &mut HashMap<String, RbxValue>) -> Result<(), DecodeError> {
    read_event!(reader, XmlEvent::StartElement { name, .. } => {
        if name.local_name != "Properties" {
            return Err(DecodeError::MalformedDocument)
        }
    });

    loop {
        let (property_type, property_name) = loop {
            match reader.next().ok_or(DecodeError::MalformedDocument)?? {
                XmlEvent::StartElement { name, mut attributes, .. } => {
                    let mut property_name = None;

                    for attribute in attributes.drain(..) {
                        if attribute.name.local_name.as_str() == "name" {
                            property_name = Some(attribute.value);
                        }
                    }

                    let property_name = property_name.ok_or(DecodeError::Message("Missing 'name' for property tag"))?;
                    break (name.local_name, property_name)
                },
                XmlEvent::EndElement { name } => {
                    if name.local_name == "Properties" {
                        return Ok(())
                    } else {
                        return Err(DecodeError::MalformedDocument)
                    }
                },
                XmlEvent::Whitespace(_) => {},
                _ => return Err(DecodeError::MalformedDocument),
            };
        };

        let value = match property_type.as_str() {
            "bool" => deserialize_bool(reader)?,
            "string" => deserialize_string(reader)?,
            "Vector3" => deserialize_vector3(reader)?,
            "Vector3int16" => deserialize_vector3int16(reader)?,
            "Vector2" => deserialize_vector2(reader)?,
            "Vector2int16" => deserialize_vector2int16(reader)?,
            "Color3" => deserialize_color3(reader)?,
            "Color3uint8" => deserialize_color3uint8(reader)?,
            "CoordinateFrame" => deserialize_cframe(reader)?,
            _ => return Err(DecodeError::Message("don't know how to decode this prop type")),
        };

        props.insert(property_name, value);

        // If there's anything left after the type deserializer kernel, then
        // stuff is broken.
        read_event!(reader, XmlEvent::EndElement { name } => {
            if name.local_name != property_type {
                return Err(DecodeError::MalformedDocument);
            }
        });
    }
}

/// Consume events from the iterator until we reach the end of the next tag.
fn eat_unknown_tag<R: Read>(reader: &mut EventIterator<R>) -> Result<(), DecodeError> {
    let mut depth = 0;

    loop {
        match reader.next().ok_or(DecodeError::MalformedDocument)?? {
            XmlEvent::StartElement { .. } => {
                depth += 1;
            },
            XmlEvent::EndElement { .. } => {
                depth -= 1;

                if depth == 0 {
                    break;
                }
            },
            _ => {},
        }
    }

    Ok(())
}

#[cfg(test)]
mod test {
    use std::collections::HashMap;
    use rbx_tree::RbxInstanceProperties;

    use super::*;

    fn new_data_model() -> RbxTree {
        let root = RbxInstanceProperties {
            name: "DataModel".to_string(),
            class_name: "DataModel".to_string(),
            properties: HashMap::new(),
        };

        RbxTree::new(root)
    }

    fn floats_approx_equal(left: f32, right: f32, epsilon: f32) -> bool {
        (left - right).abs() <= epsilon
    }

    #[test]
    fn empty_document() {
        let _ = env_logger::try_init();
        let document = r#"<roblox version="4"></roblox>"#;
        let mut tree = new_data_model();
        let root_id = tree.get_root_id();

        decode_str(&mut tree, root_id, document).unwrap();
    }

    #[test]
    fn mostly_empty() {
        let _ = env_logger::try_init();
        let document = r#"
            <roblox version="4">
                <!-- hello there! -->
                <Meta name="Trash">true</Meta>
            </roblox>
        "#;

        let mut tree = new_data_model();
        let root_id = tree.get_root_id();

        decode_str(&mut tree, root_id, document).unwrap();
    }

    #[test]
    fn top_level_garbage() {
        let _ = env_logger::try_init();
        let document = r#"
            <roblox version="4">
                <ack />
            </roblox>
        "#;

        let mut tree = new_data_model();
        let root_id = tree.get_root_id();

        assert!(decode_str(&mut tree, root_id, document).is_err());
    }

    #[test]
    fn empty_instance() {
        let _ = env_logger::try_init();
        let document = r#"
            <roblox version="4">
                <Item class="Folder" referent="hello">
                </Item>
            </roblox>
        "#;

        let mut tree = new_data_model();
        let root_id = tree.get_root_id();

        decode_str(&mut tree, root_id, document).unwrap();

        let root = tree.get_instance(root_id).unwrap();
        assert_eq!(root.get_children_ids().len(), 1);
    }

    #[test]
    fn children() {
        let _ = env_logger::try_init();
        let document = r#"
            <roblox version="4">
                <Item class="Folder" referent="hello">
                    <Properties>
                        <string name="Name">Outer</string>
                    </Properties>
                    <Item class="Folder" referent="child">
                        <Properties>
                            <string name="Name">Inner</string>
                        </Properties>
                    </Item>
                </Item>
            </roblox>
        "#;

        let mut tree = new_data_model();
        let root_id = tree.get_root_id();

        decode_str(&mut tree, root_id, document).unwrap();
        let root = tree.get_instance(root_id).unwrap();
        let first_folder = tree.get_instance(root.get_children_ids()[0]).expect("expected a child");
        let inner_folder = tree.get_instance(first_folder.get_children_ids()[0]).expect("expected a subchild");
        assert_eq!(first_folder.name, "Outer");
        assert_eq!(inner_folder.name, "Inner");
    }

    #[test]
    fn with_bool() {
        let _ = env_logger::try_init();
        let document = r#"
            <roblox version="4">
                <Item class="BoolValue" referent="hello">
                    <Properties>
                        <string name="Name">Test</string>
                        <bool name="Value">true</bool>
                    </Properties>
                </Item>
            </roblox>
        "#;

        let mut tree = new_data_model();
        let root_id = tree.get_root_id();

        decode_str(&mut tree, root_id, document).expect("should work D:");

        let descendant = tree.descendants(root_id).nth(1).unwrap();
        assert_eq!(descendant.name, "Test");
        assert_eq!(descendant.class_name, "BoolValue");
        assert_eq!(descendant.properties.get("Value"), Some(&RbxValue::Bool { value: true }));
    }

    #[test]
    fn with_vector3() {
        let _ = env_logger::try_init();
        let document = r#"
            <roblox version="4">
                <Item class="Vector3Value" referent="hello">
                    <Properties>
                        <string name="Name">Test</string>
                        <Vector3 name="Value">
                            <X>0</X>
                            <Y>0.25</Y>
                            <Z>-123.23</Z>
                        </Vector3>
                    </Properties>
                </Item>
            </roblox>
        "#;

        let mut tree = new_data_model();
        let root_id = tree.get_root_id();

        decode_str(&mut tree, root_id, document).expect("should work D:");

        let descendant = tree.descendants(root_id).nth(1).unwrap();
        assert_eq!(descendant.name, "Test");
        assert_eq!(descendant.class_name, "Vector3Value");
        assert_eq!(descendant.properties.get("Value"), Some(&RbxValue::Vector3 { value: [ 0.0, 0.25, -123.23 ] }));
    }

    #[test]
    fn with_color3() {
        let _ = env_logger::try_init();
        let document = r#"
            <roblox version="4">
                <Item class="Color3Value" referent="hello">
                    <Properties>
                        <string name="Name">Test</string>
                        <Color3 name="Value">
                            <R>0</R>
                            <G>0.25</G>
                            <B>0.75</B>
                        </Color3>
                    </Properties>
                </Item>
                <Item class="Color3Value" referent="hello">
                    <Properties>
                        <string name="Name">Test2</string>
                        <Color3 name="Value">4294934592</Color3>
                    </Properties>
                </Item>
            </roblox>
        "#;

        let mut tree = new_data_model();
        let root_id = tree.get_root_id();

        decode_str(&mut tree, root_id, document).expect("should work D:");

        for descendant in tree.descendants(root_id) {
            if descendant.name == "Test" {
                assert_eq!(descendant.properties.get("Value"), Some(&RbxValue::Color3 { value: [ 0.0, 0.25, 0.75 ] }));
            } else if descendant.name == "Test2" {
                if let Some(&RbxValue::Color3 { value }) = descendant.properties.get("Value") {
                    assert!(floats_approx_equal(value[0], 1.0, 0.001));
                    assert!(floats_approx_equal(value[1], 0.501961, 0.001));
                    assert!(floats_approx_equal(value[2], 0.250980, 0.001));
                } else {
                    panic!("value was not a Color3 or did not deserialize properly");
                }
            }
        }
    }

    #[test]
    fn with_color3uint8() {
        let _ = env_logger::try_init();
        let document = r#"
            <roblox version="4">
                <Item class="Color3Value" referent="hello">
                    <Properties>
                        <string name="Name">Test</string>
                        <Color3uint8 name="Value">4294934592</Color3uint8>
                    </Properties>
                </Item>
            </roblox>
        "#;

        let mut tree = new_data_model();
        let root_id = tree.get_root_id();

        decode_str(&mut tree, root_id, document).expect("should work D:");

        let descendant = tree.descendants(root_id).nth(1).unwrap();
        assert_eq!(descendant.name, "Test");
        assert_eq!(descendant.class_name, "Color3Value");
        assert_eq!(descendant.properties.get("Value"), Some(&RbxValue::Color3uint8 { value: [ 255, 128, 64 ] }));
    }

    #[test]
    fn with_vector2() {
        let _ = env_logger::try_init();
        let document = r#"
            <roblox version="4">
                <Item class="Vector2Value" referent="hello">
                    <Properties>
                        <string name="Name">Test</string>
                        <Vector2 name="Value">
                            <X>0</X>
                            <Y>0.5</Y>
                        </Vector2>
                    </Properties>
                </Item>
            </roblox>
        "#;

        let mut tree = new_data_model();
        let root_id = tree.get_root_id();

        decode_str(&mut tree, root_id, document).expect("should work D:");

        let descendant = tree.descendants(root_id).nth(1).unwrap();
        assert_eq!(descendant.name, "Test");
        assert_eq!(descendant.class_name, "Vector2Value");
        assert_eq!(descendant.properties.get("Value"), Some(&RbxValue::Vector2 { value: [ 0.0, 0.5 ] }));
    }

    #[test]
    fn with_cframe() {
        let _ = env_logger::try_init();
        let document = r#"
            <roblox version="4">
                <Item class="CFrameValue" referent="hello">
                    <Properties>
                        <string name="Name">Test</string>
                        <CoordinateFrame name="Value">
                            <X>0</X>
                            <Y>0.5</Y>
                            <Z>0</Z>
                            <R00>1</R00>
                            <R01>0</R01>
                            <R02>0</R02>
                            <R10>0</R10>
                            <R11>1</R11>
                            <R12>0</R12>
                            <R20>0</R20>
                            <R21>0</R21>
                            <R22>1</R22>
                        </CoordinateFrame>
                    </Properties>
                </Item>
            </roblox>
        "#;

        let mut tree = new_data_model();
        let root_id = tree.get_root_id();

        decode_str(&mut tree, root_id, document).expect("should work D:");

        let descendant = tree.descendants(root_id).nth(1).unwrap();
        assert_eq!(descendant.name, "Test");
        assert_eq!(descendant.class_name, "CFrameValue");
        assert_eq!(descendant.properties.get("Value"), Some(&RbxValue::CoordinateFrame { value: [ 0.0, 0.5, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0 ] }));
    }
}