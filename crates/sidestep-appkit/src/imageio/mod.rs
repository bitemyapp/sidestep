//! ImageIO over the codecs of the `image` crate (`crate::codec`: PNG, JPEG,
//! GIF, WebP, BMP, TIFF and ICO) and CoreGraphics' `CGImage`: the C
//! functions objc2-image-io declares for image sources and destinations,
//! and the property and option keys, exported with macOS's values.
//!
//! - [`source`]: `CGImageSource`, from data, a data provider, a file's URL
//!   or data arriving a piece at a time: the file's type, its images (and
//!   an animated GIF's or WebP's frames), their properties, and thumbnails.
//! - [`destination`]: `CGImageDestination`, writing PNG, JPEG, GIF
//!   (animated too), TIFF and BMP files to data, a URL or a data consumer.
//! - [`inspect`]: what a file's header and metadata say, as the property
//!   dictionaries ImageIO hands out; [`exif`] reads TIFF directories (a TIFF
//!   file's, or an EXIF block's); [`format`] knows the types.
//!
//! Sources and destinations are Objective-C objects of Sidestep-private
//! classes (`_SidestepCGImageSource`, `_SidestepCGImageDestination`) with
//! CoreFoundation type IDs of their own, as CoreGraphics' are. HEIC, AVIF,
//! JPEG 2000, RAW and the other types the codecs lack aren't read: a
//! source of one has no type and no images, as ImageIO's source of an
//! unknown type, and a destination of one isn't made. `CGImageMetadata`,
//! `CGAnimateImage…` and auxiliary data aren't here.

mod constants;
pub(crate) mod destination;
pub(crate) mod exif;
pub(crate) mod format;
pub(crate) mod inspect;
pub(crate) mod plist;
pub(crate) mod source;
