//! CoreText's string constants, with the values macOS gives them
//! (read at run time on macOS; several are AppKit's names, such as
//! `kCTFontAttributeName`, which is `NSFontAttributeName`'s "NSFont", so
//! attributed strings mean the same to both).

// Name keys, feature and variation keys, and baseline classes (CTFont).
sidestep_foundation::constant_string!(kCTFontCopyrightNameKey = "CTFontCopyrightName");
sidestep_foundation::constant_string!(kCTFontFamilyNameKey = "CTFontFamilyName");
sidestep_foundation::constant_string!(kCTFontSubFamilyNameKey = "CTFontSubFamilyName");
sidestep_foundation::constant_string!(kCTFontStyleNameKey = "CTFontSubFamilyName");
sidestep_foundation::constant_string!(kCTFontUniqueNameKey = "CTFontUniqueName");
sidestep_foundation::constant_string!(kCTFontFullNameKey = "CTFontFullName");
sidestep_foundation::constant_string!(kCTFontVersionNameKey = "CTFontVersionName");
sidestep_foundation::constant_string!(kCTFontPostScriptNameKey = "CTFontPostScriptName");
sidestep_foundation::constant_string!(kCTFontTrademarkNameKey = "CTFontTrademarkName");
sidestep_foundation::constant_string!(kCTFontManufacturerNameKey = "CTFontManufacturerName");
sidestep_foundation::constant_string!(kCTFontDesignerNameKey = "CTFontDesignerName");
sidestep_foundation::constant_string!(kCTFontDescriptionNameKey = "CTFontDescriptionName");
sidestep_foundation::constant_string!(kCTFontVendorURLNameKey = "CTFontVendorURLName");
sidestep_foundation::constant_string!(kCTFontDesignerURLNameKey = "CTFontDesignerURLName");
sidestep_foundation::constant_string!(kCTFontLicenseNameKey = "CTFontLicenseNameName");
sidestep_foundation::constant_string!(kCTFontLicenseURLNameKey = "CTFontLicenseURLName");
sidestep_foundation::constant_string!(kCTFontSampleTextNameKey = "CTFontSampleTextName");
sidestep_foundation::constant_string!(kCTFontPostScriptCIDNameKey = "CTFontPostScriptCIDName");
sidestep_foundation::constant_string!(kCTFontVariationAxisIdentifierKey = "NSCTVariationAxisIdentifier");
sidestep_foundation::constant_string!(kCTFontVariationAxisMinimumValueKey = "NSCTVariationAxisMinimumValue");
sidestep_foundation::constant_string!(kCTFontVariationAxisMaximumValueKey = "NSCTVariationAxisMaximumValue");
sidestep_foundation::constant_string!(kCTFontVariationAxisDefaultValueKey = "NSCTVariationAxisDefaultValue");
sidestep_foundation::constant_string!(kCTFontVariationAxisNameKey = "NSCTVariationAxisName");
sidestep_foundation::constant_string!(kCTFontVariationAxisHiddenKey = "NSCTVariationAxisHidden");
sidestep_foundation::constant_string!(kCTFontOpenTypeFeatureTag = "CTFeatureOpenTypeTag");
sidestep_foundation::constant_string!(kCTFontOpenTypeFeatureValue = "CTFeatureOpenTypeValue");
sidestep_foundation::constant_string!(kCTFontFeatureTypeIdentifierKey = "CTFeatureTypeIdentifier");
sidestep_foundation::constant_string!(kCTFontFeatureTypeNameKey = "CTFeatureTypeName");
sidestep_foundation::constant_string!(kCTFontFeatureTypeExclusiveKey = "CTFeatureTypeExclusive");
sidestep_foundation::constant_string!(kCTFontFeatureTypeSelectorsKey = "CTFeatureTypeSelectors");
sidestep_foundation::constant_string!(kCTFontFeatureSelectorIdentifierKey = "CTFeatureSelectorIdentifier");
sidestep_foundation::constant_string!(kCTFontFeatureSelectorNameKey = "CTFeatureSelectorName");
sidestep_foundation::constant_string!(kCTFontFeatureSelectorDefaultKey = "CTFeatureSelectorDefault");
sidestep_foundation::constant_string!(kCTFontFeatureSelectorSettingKey = "CTFeatureSelectorSetting");
sidestep_foundation::constant_string!(kCTFontFeatureSampleTextKey = "CTFeatureSampleText");
sidestep_foundation::constant_string!(kCTFontFeatureTooltipTextKey = "CTFeatureTooltipText");
sidestep_foundation::constant_string!(kCTBaselineClassRoman = "CTBaselineClassRoman");
sidestep_foundation::constant_string!(kCTBaselineClassIdeographicCentered = "CTBaselineClassIdeographicCentered");
sidestep_foundation::constant_string!(kCTBaselineClassIdeographicLow = "CTBaselineClassIdeographicLow");
sidestep_foundation::constant_string!(kCTBaselineClassIdeographicHigh = "CTBaselineClassIdeographicHigh");
sidestep_foundation::constant_string!(kCTBaselineClassHanging = "CTBaselineClassHanging");
sidestep_foundation::constant_string!(kCTBaselineClassMath = "CTBaselineClassMath");
sidestep_foundation::constant_string!(kCTBaselineReferenceFont = "CTBaselineReferenceFont");
sidestep_foundation::constant_string!(kCTBaselineOriginalFont = "CTBaselineOriginalFont");

// Font collection options.
sidestep_foundation::constant_string!(
    kCTFontCollectionRemoveDuplicatesOption = "NSCTFontCollectionRemoveDuplicatesOption"
);
sidestep_foundation::constant_string!(
    kCTFontCollectionIncludeDisabledFontsOption = "NSCTFontCollectionIncludeDisabledFontsOption"
);
sidestep_foundation::constant_string!(
    kCTFontCollectionDisallowAutoActivationOption = "NSCTFontCollectionDisallowAutoActivationOption"
);

// Font descriptor attributes and matching keys.
sidestep_foundation::constant_string!(kCTFontURLAttribute = "NSCTFontFileURLAttribute");
sidestep_foundation::constant_string!(kCTFontNameAttribute = "NSFontNameAttribute");
sidestep_foundation::constant_string!(kCTFontDisplayNameAttribute = "NSFontVisibleNameAttribute");
sidestep_foundation::constant_string!(kCTFontFamilyNameAttribute = "NSFontFamilyAttribute");
sidestep_foundation::constant_string!(kCTFontStyleNameAttribute = "NSFontFaceAttribute");
sidestep_foundation::constant_string!(kCTFontTraitsAttribute = "NSCTFontTraitsAttribute");
sidestep_foundation::constant_string!(kCTFontVariationAttribute = "NSCTFontVariationAttribute");
sidestep_foundation::constant_string!(kCTFontVariationAxesAttribute = "NSCTFontVariationAxesAttribute");
sidestep_foundation::constant_string!(kCTFontSizeAttribute = "NSFontSizeAttribute");
sidestep_foundation::constant_string!(kCTFontMatrixAttribute = "NSCTFontMatrixAttribute");
sidestep_foundation::constant_string!(kCTFontCascadeListAttribute = "NSCTFontCascadeListAttribute");
sidestep_foundation::constant_string!(kCTFontCharacterSetAttribute = "NSCTFontCharacterSetAttribute");
sidestep_foundation::constant_string!(kCTFontLanguagesAttribute = "NSCTFontLanguagesAttribute");
sidestep_foundation::constant_string!(kCTFontBaselineAdjustAttribute = "NSCTFontBaselineAdjustAttribute");
sidestep_foundation::constant_string!(kCTFontMacintoshEncodingsAttribute = "NSCTFontMacintoshEncodingsAttribute");
sidestep_foundation::constant_string!(kCTFontFeaturesAttribute = "NSCTFontFeaturesAttribute");
sidestep_foundation::constant_string!(kCTFontFeatureSettingsAttribute = "NSCTFontFeatureSettingsAttribute");
sidestep_foundation::constant_string!(kCTFontFixedAdvanceAttribute = "NSCTFontFixedAdvanceAttribute");
sidestep_foundation::constant_string!(kCTFontOrientationAttribute = "NSCTFontOrientationAttribute");
sidestep_foundation::constant_string!(kCTFontFormatAttribute = "NSCTFontFormatAttribute");
sidestep_foundation::constant_string!(kCTFontRegistrationScopeAttribute = "NSCTFontRegistrationScopeAttribute");
sidestep_foundation::constant_string!(kCTFontPriorityAttribute = "NSCTFontPriorityAttribute");
sidestep_foundation::constant_string!(kCTFontEnabledAttribute = "NSCTFontEnabledAttribute");
sidestep_foundation::constant_string!(kCTFontDownloadableAttribute = "NSCTFontDownloadableAttribute");
sidestep_foundation::constant_string!(kCTFontDownloadedAttribute = "NSCTFontDownloadedAttribute");
sidestep_foundation::constant_string!(kCTFontOpticalSizeAttribute = "NSCTFontOpticalSizeAttribute");
sidestep_foundation::constant_string!(
    kCTFontDescriptorMatchingSourceDescriptor = "CTFontDescriptorMatchingSourceDescriptor"
);
sidestep_foundation::constant_string!(kCTFontDescriptorMatchingDescriptors = "CTFontDescriptorMatchingDescriptors");
sidestep_foundation::constant_string!(kCTFontDescriptorMatchingResult = "CTFontDescriptorMatchingResult");
sidestep_foundation::constant_string!(kCTFontDescriptorMatchingPercentage = "CTFontDescriptorMatchingPercentage");
sidestep_foundation::constant_string!(
    kCTFontDescriptorMatchingCurrentAssetSize = "CTFontDescriptorMatchingCurrentAssetSize"
);
sidestep_foundation::constant_string!(
    kCTFontDescriptorMatchingTotalDownloadedSize = "CTFontDescriptorMatchingTotalDownloadedSize"
);
sidestep_foundation::constant_string!(
    kCTFontDescriptorMatchingTotalAssetSize = "CTFontDescriptorMatchingTotalAssetSize"
);
sidestep_foundation::constant_string!(kCTFontDescriptorMatchingError = "CTFontDescriptorMatchingError");

// The font manager.
sidestep_foundation::constant_string!(kCTFontRegistrationUserInfoAttribute = "CTFontRegistrationUserInfoAttribute");
sidestep_foundation::constant_string!(kCTFontManagerBundleIdentifier = "com.apple.CoreText");
sidestep_foundation::constant_string!(
    kCTFontManagerRegisteredFontsChangedNotification = "CTFontManagerFontChangedNotification"
);

// The font manager's errors.
sidestep_foundation::constant_string!(kCTFontManagerErrorDomain = "com.apple.CoreText.CTFontManagerErrorDomain");
sidestep_foundation::constant_string!(kCTFontManagerErrorFontURLsKey = "CTFontManagerErrorFontURLs");
sidestep_foundation::constant_string!(kCTFontManagerErrorFontDescriptorsKey = "CTFontManagerErrorFontDescriptors");
sidestep_foundation::constant_string!(kCTFontManagerErrorFontAssetNameKey = "CTFontManagerErrorFontAssetNameKey");

// Trait keys.
sidestep_foundation::constant_string!(kCTFontSymbolicTrait = "NSCTFontSymbolicTrait");
sidestep_foundation::constant_string!(kCTFontWeightTrait = "NSCTFontWeightTrait");
sidestep_foundation::constant_string!(kCTFontWidthTrait = "NSCTFontProportionTrait");
sidestep_foundation::constant_string!(kCTFontSlantTrait = "NSCTFontSlantTrait");

// Frame attributes.
sidestep_foundation::constant_string!(kCTFrameProgressionAttributeName = "CTFrameProgression");
sidestep_foundation::constant_string!(kCTFramePathFillRuleAttributeName = "CTFramePathFillRule");
sidestep_foundation::constant_string!(kCTFramePathWidthAttributeName = "CTFramePathWidth");
sidestep_foundation::constant_string!(kCTFrameClippingPathsAttributeName = "CTFrameClippingPaths");
sidestep_foundation::constant_string!(kCTFramePathClippingPathAttributeName = "CTFramePathClippingPath");

// Ruby annotation attributes.
sidestep_foundation::constant_string!(kCTRubyAnnotationSizeFactorAttributeName = "CTRubyAnnotationSizeFactor");
sidestep_foundation::constant_string!(kCTRubyAnnotationScaleToFitAttributeName = "CTRubyAnnotationScaleToFit");

// String attributes: CoreText's names, some of them AppKit's.
sidestep_foundation::constant_string!(kCTFontAttributeName = "NSFont");
sidestep_foundation::constant_string!(kCTForegroundColorFromContextAttributeName = "CTForegroundColorFromContext");
sidestep_foundation::constant_string!(kCTKernAttributeName = "NSKern");
sidestep_foundation::constant_string!(kCTTrackingAttributeName = "CTTracking");
sidestep_foundation::constant_string!(kCTLigatureAttributeName = "NSLigature");
sidestep_foundation::constant_string!(kCTForegroundColorAttributeName = "CTForegroundColor");
sidestep_foundation::constant_string!(kCTBackgroundColorAttributeName = "CTBackgroundColor");
sidestep_foundation::constant_string!(kCTParagraphStyleAttributeName = "NSParagraphStyle");
sidestep_foundation::constant_string!(kCTStrokeWidthAttributeName = "NSStrokeWidth");
sidestep_foundation::constant_string!(kCTStrokeColorAttributeName = "CTStrokeColor");
sidestep_foundation::constant_string!(kCTUnderlineStyleAttributeName = "NSUnderline");
sidestep_foundation::constant_string!(kCTSuperscriptAttributeName = "CTSuperscript");
sidestep_foundation::constant_string!(kCTUnderlineColorAttributeName = "CTUnderlineColor");
sidestep_foundation::constant_string!(kCTVerticalFormsAttributeName = "CTVerticalForms");
sidestep_foundation::constant_string!(kCTHorizontalInVerticalFormsAttributeName = "CTHorizontalInVerticalForms");
sidestep_foundation::constant_string!(kCTGlyphInfoAttributeName = "NSGlyphInfo");
sidestep_foundation::constant_string!(kCTCharacterShapeAttributeName = "NSCharacterShape");
sidestep_foundation::constant_string!(kCTLanguageAttributeName = "NSLanguage");
sidestep_foundation::constant_string!(kCTRunDelegateAttributeName = "CTRunDelegate");
sidestep_foundation::constant_string!(kCTBaselineClassAttributeName = "CTBaselineClass");
sidestep_foundation::constant_string!(kCTBaselineInfoAttributeName = "CTBaselineInfo");
sidestep_foundation::constant_string!(kCTBaselineReferenceInfoAttributeName = "CTBaselineReferenceInfo");
sidestep_foundation::constant_string!(kCTBaselineOffsetAttributeName = "CTBaselineOffset");
sidestep_foundation::constant_string!(kCTWritingDirectionAttributeName = "NSWritingDirection");
sidestep_foundation::constant_string!(kCTRubyAnnotationAttributeName = "CTRubyAnnotation");
sidestep_foundation::constant_string!(kCTAdaptiveImageProviderAttributeName = "CTAdaptiveImageProvider");

// Text tab options.
sidestep_foundation::constant_string!(kCTTabColumnTerminatorsAttributeName = "NSTabColumnTerminatorsAttributeName");

// Typesetter options.
sidestep_foundation::constant_string!(
    kCTTypesetterOptionAllowUnboundedLayout = "CTTypesetterOptionAllowUnboundedLayout"
);
sidestep_foundation::constant_string!(
    kCTTypesetterOptionDisableBidiProcessing = "CTTypesetterOptionDisableBidiProcessing"
);
sidestep_foundation::constant_string!(
    kCTTypesetterOptionForcedEmbeddingLevel = "CTTypesetterOptionForcedEmbeddingLevel"
);
